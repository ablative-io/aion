//! Replay driver to the resume point.
//!
//! Replay is an orchestration layer above [`Resolver`]. AE owns the workflow process and feeds the
//! commands it emits into this driver. While history satisfies commands, the driver returns recorded
//! resolutions and advances the deterministic timestamp. The first command history cannot satisfy is
//! reported as the resume-live point for AE's AD-007 handoff.

use aion_core::{Event, Payload, RunId, WorkflowError, WorkflowId};
use chrono::{DateTime, Utc};

use crate::durability::{
    Command, DeterminismContext, DurabilityError, HistoryCursor, LiveExecutor, Recorder,
    Resolution, ResolvedCommand, Resolver,
};

/// Stateful replay driver for one workflow history.
pub struct Replay {
    resolver: Resolver,
    determinism: DeterminismContext,
    terminal: Option<ReplayTerminal>,
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
        let started_at = workflow_started_at(workflow_id, &history)?;
        let terminal = terminal_from_history(&history);
        let cursor = HistoryCursor::new(history)?;
        let resolver = Resolver::new(workflow_id.clone(), cursor);
        let determinism = DeterminismContext::new(started_at, workflow_id, run_id);

        Ok(Self {
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

    /// Returns the determinism context for callers that need deterministic random output.
    #[must_use]
    pub const fn determinism(&self) -> &DeterminismContext {
        &self.determinism
    }

    /// Mutably borrows the determinism context for callers that need deterministic random output.
    pub fn determinism_mut(&mut self) -> &mut DeterminismContext {
        &mut self.determinism
    }

    /// Resolves the next engine-supplied workflow command against recorded history.
    ///
    /// # Errors
    ///
    /// Returns resolver errors, including typed non-determinism violations at the mismatch point.
    pub fn step(&mut self, command: Command) -> Result<ReplayStep, DurabilityError> {
        match self.resolver.resolve_with_consumed(command)? {
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
                if let Some(terminal) = &self.terminal {
                    Ok(ReplayStep::Terminal(terminal.clone()))
                } else {
                    Ok(ReplayStep::ResumeLive)
                }
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
            match self.step(command.clone())? {
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
            Ok(ReplayOutcome::Terminal { terminal, recorded })
        } else {
            Ok(ReplayOutcome::AwaitingCommand { recorded })
        }
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

fn terminal_from_history(history: &[Event]) -> Option<ReplayTerminal> {
    history.iter().rev().find_map(|event| match event {
        Event::WorkflowCompleted { result, .. } => Some(ReplayTerminal::Completed(result.clone())),
        Event::WorkflowFailed { error, .. } => Some(ReplayTerminal::Failed(error.clone())),
        Event::WorkflowCancelled { reason, .. } => Some(ReplayTerminal::Cancelled(reason.clone())),
        Event::WorkflowTimedOut { timeout, .. } => Some(ReplayTerminal::TimedOut(timeout.clone())),
        _ => None,
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
            },
            aion_core::Event::ActivityScheduled {
                envelope: envelope(2, 20)?,
                activity_id: activity_id.clone(),
                activity_type: "activity".to_owned(),
                input: payload("activity-input")?,
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

    #[test]
    fn step_returns_recorded_resolutions_and_advances_now() -> TestResult {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let mut replay = Replay::new(&workflow_id, &run_id, history()?)?;
        assert_eq!(replay.now(), timestamp(10)?);

        assert_eq!(
            replay.step(activity_command()?)?,
            ReplayStep::Recorded(Resolution::ActivityCompleted(payload("activity-result")?))
        );
        assert_eq!(replay.now(), timestamp(30)?);

        assert_eq!(
            replay.step(timer_command()?)?,
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
}
