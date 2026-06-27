//! Replay driver to the resume point.
//!
//! Replay is an orchestration layer above [`Resolver`]. AE owns the workflow process and feeds the
//! commands it emits into this driver. While history satisfies commands, the driver returns recorded
//! resolutions and advances the deterministic timestamp. The first command history cannot satisfy is
//! reported as the resume-live point for AE's AD-007 handoff.

use aion_core::{Event, Payload, RunId, WorkflowError, WorkflowId};
use chrono::{DateTime, Utc};

use crate::durability::{
    Command, DeterminismContext, DurabilityError, HistoryCursor, LiveExecutor, NonDeterminismError,
    Recorder, Resolution, ResolvedCommand, Resolver,
};

/// Stateful replay driver for one workflow history.
pub struct Replay {
    workflow_id: WorkflowId,
    resolver: Resolver,
    determinism: DeterminismContext,
    terminal: Option<TerminalRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalRecord {
    seq: u64,
    terminal: ReplayTerminal,
}

impl Replay {
    /// Assembles replay state from a workflow id, run id, and ordered recorded history.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError::HistoryShape`] if the history is unordered or lacks a
    /// `WorkflowStarted` event for the requested workflow.
    pub fn new(
        workflow_id: &WorkflowId,
        run_id: &RunId,
        history: Vec<Event>,
    ) -> Result<Self, DurabilityError> {
        let history = crate::durability::current_run_segment(history, run_id)?;
        let started_at = workflow_started_at(workflow_id, &history)?;
        let terminal = terminal_from_history(workflow_id, &history);
        let cursor = HistoryCursor::new(history)?;
        let resolver = Resolver::new(workflow_id.clone(), cursor);
        let determinism = DeterminismContext::new(started_at);

        Ok(Self {
            workflow_id: workflow_id.clone(),
            resolver,
            determinism,
            terminal,
        })
    }

    /// Assembles replay state while accepting the AD-007 handoff collaborators.
    ///
    /// Replay keeps live execution out of the recorded path; these handles are accepted so AE can
    /// construct replay and retain the same collaborators for the first [`ReplayStep::ResumeLive`].
    ///
    /// # Errors
    ///
    /// Returns the same history-shape errors as [`Self::new`].
    pub fn with_handoff(
        workflow_id: &WorkflowId,
        run_id: &RunId,
        history: Vec<Event>,
        recorder: &Recorder,
        executor: &dyn LiveExecutor,
    ) -> Result<Self, DurabilityError> {
        let _ = recorder.workflow_id();
        let _ = executor;
        Self::new(workflow_id, run_id, history)
    }

    /// Returns the deterministic workflow-visible timestamp currently applied by replay.
    #[must_use]
    pub const fn now(&self) -> DateTime<Utc> {
        self.determinism.now()
    }

    /// Resolves the next engine-supplied workflow command against recorded history.
    ///
    /// # Errors
    ///
    /// Returns resolver errors, including typed non-determinism violations at the mismatch point.
    pub fn step(&mut self, command: &Command) -> Result<ReplayStep, DurabilityError> {
        match self.resolver.resolve_with_consumed(command.clone())? {
            ResolvedCommand::Recorded {
                resolution,
                recorded_at,
            } => {
                self.determinism.advance_to_recorded_at(recorded_at);
                Ok(ReplayStep::Recorded(resolution))
            }
            ResolvedCommand::ResumeLive { recorded_at } => {
                if let Some(recorded_at) = recorded_at {
                    self.determinism.advance_to_recorded_at(recorded_at);
                }
                self.resume_or_terminal(command)
            }
        }
    }

    /// Drives replay over an iterator of commands until a resume-live or terminal point appears.
    ///
    /// Recorded resolutions are included in the returned outcome so AE can return them to the
    /// workflow process in order before acting on the handoff.
    ///
    /// # Errors
    ///
    /// Returns resolver errors, including typed non-determinism violations at the mismatch point.
    pub fn drive<I>(&mut self, commands: I) -> Result<ReplayOutcome, DurabilityError>
    where
        I: IntoIterator<Item = Command>,
    {
        let mut recorded = Vec::new();
        for (command_index, command) in commands.into_iter().enumerate() {
            match self.step(&command)? {
                ReplayStep::Recorded(resolution) => recorded.push(resolution),
                ReplayStep::ResumeLive => {
                    return Ok(ReplayOutcome::ResumeLive {
                        command_index,
                        command,
                        recorded,
                    });
                }
                ReplayStep::Terminal(terminal) => {
                    return Ok(ReplayOutcome::Terminal { terminal, recorded });
                }
            }
        }

        if let Some(terminal) = self.terminal.clone() {
            Ok(ReplayOutcome::Terminal {
                terminal: terminal.terminal,
                recorded,
            })
        } else {
            Ok(ReplayOutcome::AwaitingCommand { recorded })
        }
    }

    fn resume_or_terminal(&self, command: &Command) -> Result<ReplayStep, DurabilityError> {
        let Some(terminal) = &self.terminal else {
            return Ok(ReplayStep::ResumeLive);
        };

        if let (Command::CompleteWorkflow { result }, ReplayTerminal::Completed(recorded_result)) =
            (command, &terminal.terminal)
        {
            if result == recorded_result {
                return Ok(ReplayStep::Terminal(terminal.terminal.clone()));
            }
        }

        Err(NonDeterminismError {
            workflow_id: self.workflow_id.clone(),
            seq: terminal.seq,
            expected: format!("{command:?}"),
            found: format!("terminal {:?}", terminal.terminal),
        }
        .into())
    }
}

/// Outcome of a single replay step.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplayStep {
    /// Command was satisfied from recorded history.
    Recorded(Resolution),
    /// This command is the resume-live handoff point.
    ResumeLive,
    /// Recorded history is terminal; no live handoff should be performed.
    Terminal(ReplayTerminal),
}

/// Outcome of driving replay across a command stream.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplayOutcome {
    /// All supplied commands were recorded, but the workflow process has not yet reached terminal or
    /// handoff. AE should feed the next command when it is available.
    AwaitingCommand {
        /// Recorded resolutions returned before commands were exhausted.
        recorded: Vec<Resolution>,
    },
    /// The first command absent from recorded history; AE should continue live from this command.
    ResumeLive {
        /// Zero-based command index of the resume point.
        command_index: usize,
        /// Command that must be executed live by AE.
        command: Command,
        /// Recorded resolutions returned before the resume point.
        recorded: Vec<Resolution>,
    },
    /// Replay reached a recorded terminal workflow state with no live handoff.
    Terminal {
        /// Recorded terminal event state.
        terminal: ReplayTerminal,
        /// Recorded resolutions returned before terminal state was observed.
        recorded: Vec<Resolution>,
    },
}

/// Terminal workflow state recorded in history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayTerminal {
    /// Workflow completed successfully with a recorded result.
    Completed(Payload),
    /// Workflow failed terminally with a recorded error.
    Failed(WorkflowError),
    /// Workflow was cancelled with a recorded reason.
    Cancelled(String),
    /// Workflow timed out with a recorded timeout descriptor.
    TimedOut(String),
    /// Workflow continued as a new run with a carried input payload.
    ContinuedAsNew(Payload),
}

fn workflow_started_at(
    workflow_id: &WorkflowId,
    history: &[Event],
) -> Result<DateTime<Utc>, DurabilityError> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted { .. } if event.workflow_id() == workflow_id => {
                Some(*event.recorded_at())
            }
            _ => None,
        })
        .ok_or_else(|| DurabilityError::HistoryShape {
            reason: format!("history for workflow {workflow_id} lacks WorkflowStarted"),
        })
}

fn terminal_from_history(workflow_id: &WorkflowId, history: &[Event]) -> Option<TerminalRecord> {
    // Reset-aware: a reopen (WorkflowReopened) supersedes the run's prior terminal,
    // so a reopened run has no replay terminal and the reopened command hands off
    // live (ResumeLive) instead of being rejected as non-determinism.
    let event = aion_core::current_lease_terminal(history)?;
    if event.workflow_id() != workflow_id {
        return None;
    }

    let terminal = match event {
        Event::WorkflowCompleted { result, .. } => ReplayTerminal::Completed(result.clone()),
        Event::WorkflowFailed { error, .. } => ReplayTerminal::Failed(error.clone()),
        Event::WorkflowCancelled { reason, .. } => ReplayTerminal::Cancelled(reason.clone()),
        Event::WorkflowTimedOut { timeout, .. } => ReplayTerminal::TimedOut(timeout.clone()),
        Event::WorkflowContinuedAsNew { input, .. } => {
            ReplayTerminal::ContinuedAsNew(input.clone())
        }
        _ => return None,
    };

    Some(TerminalRecord {
        seq: event.seq(),
        terminal,
    })
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityId, EventEnvelope, Payload, RunId, TimerId, WorkflowId};
    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{Replay, ReplayOutcome, ReplayStep, ReplayTerminal};
    use crate::durability::{Command, CorrelationKey, Resolution};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(Uuid::from_u128(1))
    }

    fn run_id() -> RunId {
        RunId::new(Uuid::from_u128(2))
    }

    fn timestamp(seconds: i64) -> TestResult<DateTime<Utc>> {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .ok_or_else(|| format!("invalid timestamp {seconds}").into())
    }

    fn envelope(seq: u64, seconds: i64) -> TestResult<EventEnvelope> {
        Ok(EventEnvelope {
            seq,
            recorded_at: timestamp(seconds)?,
            workflow_id: workflow_id(),
        })
    }

    fn payload(label: &str) -> TestResult<Payload> {
        Ok(Payload::from_json(&json!({ "label": label }))?)
    }

    fn history() -> TestResult<Vec<aion_core::Event>> {
        let activity_id = ActivityId::from_sequence_position(0);
        let timer_id = TimerId::anonymous(3);
        Ok(vec![
            aion_core::Event::WorkflowStarted {
                envelope: envelope(1, 10)?,
                workflow_type: "workflow".to_owned(),
                input: payload("input")?,
                run_id: run_id(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
            aion_core::Event::ActivityScheduled {
                envelope: envelope(2, 20)?,
                activity_id: activity_id.clone(),
                activity_type: "activity".to_owned(),
                input: payload("activity-input")?,
                task_queue: String::from("default"),
                node: None,
            },
            aion_core::Event::ActivityCompleted {
                envelope: envelope(3, 30)?,
                activity_id,
                result: payload("activity-result")?,
            },
            aion_core::Event::TimerStarted {
                envelope: envelope(4, 40)?,
                timer_id: timer_id.clone(),
                fire_at: timestamp(100)?,
            },
            aion_core::Event::TimerFired {
                envelope: envelope(5, 50)?,
                timer_id,
            },
            aion_core::Event::WorkflowCompleted {
                envelope: envelope(6, 60)?,
                result: payload("workflow-result")?,
            },
        ])
    }

    fn activity_command() -> TestResult<Command> {
        Ok(Command::RunActivity {
            key: CorrelationKey::Activity(0),
            activity_type: "activity".to_owned(),
            input: payload("activity-input")?,
        })
    }

    fn timer_command() -> TestResult<Command> {
        Ok(Command::StartTimer {
            key: CorrelationKey::Timer(TimerId::anonymous(3)),
            fire_at: timestamp(100)?,
        })
    }

    fn reopened_history() -> TestResult<Vec<aion_core::Event>> {
        let activity_id = ActivityId::from_sequence_position(0);
        Ok(vec![
            aion_core::Event::WorkflowStarted {
                envelope: envelope(1, 10)?,
                workflow_type: "workflow".to_owned(),
                input: payload("input")?,
                run_id: run_id(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
            aion_core::Event::ActivityScheduled {
                envelope: envelope(2, 20)?,
                activity_id: activity_id.clone(),
                activity_type: "activity".to_owned(),
                input: payload("activity-input")?,
                task_queue: String::from("default"),
                node: None,
            },
            aion_core::Event::ActivityFailed {
                envelope: envelope(3, 30)?,
                activity_id: activity_id.clone(),
                error: aion_core::ActivityError {
                    kind: aion_core::ActivityErrorKind::Terminal,
                    message: "boom".to_owned(),
                    details: None,
                },
                attempt: 1,
            },
            aion_core::Event::WorkflowFailed {
                envelope: envelope(4, 40)?,
                error: aion_core::WorkflowError {
                    message: "boom".to_owned(),
                    details: None,
                },
            },
            aion_core::Event::WorkflowReopened {
                envelope: envelope(5, 50)?,
                run_id: run_id(),
                reopened: vec![activity_id],
            },
        ])
    }

    #[test]
    fn reopened_activity_resolves_live_not_terminal() -> TestResult {
        // Regression for the reopen-foundation critical path: a reopened run's
        // terminal must be superseded so the reopened activity hands off live
        // (ResumeLive) instead of being rejected as a non-determinism violation.
        let workflow_id = workflow_id();
        let run_id = run_id();
        let mut replay = Replay::new(&workflow_id, &run_id, reopened_history()?)?;

        let step = replay.step(&activity_command()?)?;
        assert_eq!(
            step,
            ReplayStep::ResumeLive,
            "a reopened activity must re-dispatch live, not replay its superseded failure"
        );
        Ok(())
    }

    #[test]
    fn step_returns_recorded_resolutions_and_advances_now() -> TestResult {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let mut replay = Replay::new(&workflow_id, &run_id, history()?)?;
        assert_eq!(replay.now(), timestamp(10)?);

        assert_eq!(
            replay.step(&activity_command()?)?,
            ReplayStep::Recorded(Resolution::ActivityCompleted(payload("activity-result")?))
        );
        assert_eq!(replay.now(), timestamp(30)?);

        assert_eq!(
            replay.step(&timer_command()?)?,
            ReplayStep::Recorded(Resolution::TimerFired)
        );
        assert_eq!(replay.now(), timestamp(50)?);
        Ok(())
    }

    #[test]
    fn drive_reports_terminal_history_without_resume_live() -> TestResult {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let mut replay = Replay::new(&workflow_id, &run_id, history()?)?;

        let outcome = replay.drive([activity_command()?, timer_command()?])?;

        assert_eq!(
            outcome,
            ReplayOutcome::Terminal {
                terminal: ReplayTerminal::Completed(payload("workflow-result")?),
                recorded: vec![
                    Resolution::ActivityCompleted(payload("activity-result")?),
                    Resolution::TimerFired,
                ],
            }
        );
        Ok(())
    }

    #[test]
    fn ignores_terminal_events_for_other_workflows() -> TestResult {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let other_workflow_id = WorkflowId::new(Uuid::from_u128(99));
        let history = vec![
            aion_core::Event::WorkflowStarted {
                envelope: envelope(1, 10)?,
                workflow_type: "workflow".to_owned(),
                input: payload("input")?,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
            aion_core::Event::WorkflowCompleted {
                envelope: EventEnvelope {
                    seq: 2,
                    recorded_at: timestamp(20)?,
                    workflow_id: other_workflow_id,
                },
                result: payload("other-workflow-result")?,
            },
        ];
        let mut replay = Replay::new(&workflow_id, &run_id, history)?;
        let command = activity_command()?;

        let outcome = replay.drive([command.clone()])?;

        assert_eq!(
            outcome,
            ReplayOutcome::ResumeLive {
                command_index: 0,
                command,
                recorded: Vec::new(),
            }
        );
        Ok(())
    }

    #[test]
    fn terminal_history_accepts_matching_complete_workflow_command() -> TestResult {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let mut replay = Replay::new(&workflow_id, &run_id, history()?)?;
        replay.step(&activity_command()?)?;
        replay.step(&timer_command()?)?;

        let step = replay.step(&Command::CompleteWorkflow {
            result: payload("workflow-result")?,
        })?;

        assert_eq!(
            step,
            ReplayStep::Terminal(ReplayTerminal::Completed(payload("workflow-result")?))
        );
        Ok(())
    }

    #[test]
    fn terminal_history_rejects_extra_non_terminal_command() -> TestResult {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let mut replay = Replay::new(&workflow_id, &run_id, history()?)?;
        replay.step(&activity_command()?)?;
        replay.step(&timer_command()?)?;

        let error = replay.step(&activity_command()?).err();

        assert!(matches!(
            error,
            Some(crate::durability::DurabilityError::NonDeterminism(_))
        ));
        Ok(())
    }
}
