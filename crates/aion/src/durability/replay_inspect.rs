//! Time-travel inspection lens over a recorded run (WA-004).
//!
//! This is a read-only projection over data the engine already records. It adds
//! no second store, no new persistence, and no `Event`-schema change (CN5,
//! ADR-007, P6). Per event it surfaces the workflow-visible state projection and
//! the recorded `now()` — the event's recorded timestamp, the authoritative
//! determinism clock (never wall-clock time, exactly the value the production
//! `now()` NIF serves). On a [`NonDeterminismError`] it surfaces the exact
//! divergent command (expected vs found at the sequence) the resolver already
//! computes — never recomputed here (C18).
//!
//! The state projection reuses the production replay path: it reconstructs the
//! command stream the engine fed the resolver from the recorded events and
//! drives the real [`Replay`] over it, so the resolutions come from replay and
//! not a parallel engine. The what-if re-run forks the same path from a chosen
//! event with a mocked outcome, entirely in memory, and reports the resulting
//! path.
//!
//! ## Random is a draw-ordinal projection, not a per-event field
//!
//! Workflow-visible `random()` is **not** recorded and **not** attached per
//! event. The production random path is the determinism NIF
//! ([`crate::runtime::nif_determinism`]): `workflow.random()` /
//! `workflow.random_int()` draw `deterministic_float` / `deterministic_i64`
//! keyed by a per-call *draw ordinal* the workflow handle hands out, advanced
//! once per `random()` call the workflow code actually makes. The number and
//! event-positions of those draws exist only while the workflow code runs; they
//! are not derivable from the event log alone (no `Random` event variant
//! exists, by design — random is deterministic, not recorded).
//!
//! So the lens does **not** invent a per-event random stream. Instead it exposes
//! a [`RandomDrawProjection`] bound to the run's `(WorkflowId, RunId)` that
//! computes, for any draw ordinal `n`, the *exact* value the production
//! `random()` / `random_int()` path serves at that ordinal — by calling the
//! same `deterministic_float` / `deterministic_i64` the NIF calls. The true
//! per-step draw *count* is recoverable only by driving the workflow code in an
//! instrumented in-VM replay; that faithful lens is deferred (see the brief's
//! open issue) and this projection never fabricates a count it cannot know.

use aion_core::{ActivityError, Event, Payload, RunId, WorkflowError, WorkflowId};
use chrono::{DateTime, Utc};

use crate::durability::{
    Command, CorrelationKey, DurabilityError, NON_DETERMINISM_WORKFLOW_ERROR_PREFIX,
    NonDeterminismError, Replay, ReplayStep, ReplayTerminal, Resolution,
    correlation::correlation_keys_for_history, current_run_segment,
};
use crate::runtime::nif_determinism::{deterministic_float, deterministic_i64};

/// Complete inspection of one recorded run, projected from history and replay.
#[derive(Clone, Debug, PartialEq)]
pub struct RunInspection {
    /// Workflow whose history was inspected.
    pub workflow_id: WorkflowId,
    /// Run whose segment was projected.
    pub run_id: RunId,
    /// One projected step per recorded event in the run segment, in order.
    pub steps: Vec<InspectStep>,
    /// The run's deterministic `random()` draw-ordinal projection.
    ///
    /// This computes the value the production `random()` / `random_int()` path
    /// serves at a given draw ordinal for this `(WorkflowId, RunId)`. It is a
    /// projection, not a per-event field: see the module docs for why the lens
    /// cannot attach a random draw to each event without running the workflow
    /// code in-VM.
    pub random: RandomDrawProjection,
    /// The divergent command at the run's non-determinism fault, when one exists.
    pub divergence: Option<DivergentCommand>,
}

/// The deterministic `random()` draw-ordinal projection for one run.
///
/// Bound to the run's `(WorkflowId, RunId)`, it reproduces — for any draw
/// ordinal — the exact value the production determinism NIF serves, by calling
/// the same `deterministic_float` / `deterministic_i64` the NIF calls. Draw
/// ordinals start at `0` (the first `workflow.random()` call a run makes draws
/// ordinal `0`), matching the handle's pre-increment sequence counter.
///
/// This carries no draw count: the number of draws a run makes is workflow-code
/// dependent and unrecoverable from history alone (module docs). It answers
/// "what would `random()` return at ordinal `n`?", never "how many draws did
/// step `k` make?".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RandomDrawProjection {
    workflow_id: WorkflowId,
    run_id: RunId,
}

impl RandomDrawProjection {
    /// Binds the projection to a run.
    #[must_use]
    const fn new(workflow_id: WorkflowId, run_id: RunId) -> Self {
        Self {
            workflow_id,
            run_id,
        }
    }

    /// The `f64` in `[0.0, 1.0)` `workflow.random()` returns at draw `ordinal`.
    ///
    /// This is byte-for-byte the value the production `random()` NIF serves at
    /// that ordinal for this run — it calls the same `deterministic_float`.
    #[must_use]
    pub fn random_at(&self, ordinal: u64) -> f64 {
        deterministic_float(&self.workflow_id, &self.run_id, ordinal)
    }

    /// The `i64` in `[min, max]` `workflow.random_int(min, max)` returns at draw
    /// `ordinal`.
    ///
    /// This is the value the production `random_int` NIF serves at that ordinal
    /// for this run — it calls the same `deterministic_i64`.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError::HistoryShape`] when `min > max`, mirroring the
    /// NIF's loud rejection of an inverted range (no silent clamping).
    pub fn random_int_at(&self, ordinal: u64, min: i64, max: i64) -> Result<i64, DurabilityError> {
        if min > max {
            return Err(DurabilityError::HistoryShape {
                reason: format!(
                    "random_int_at range is inverted: min {min} is greater than max {max}"
                ),
            });
        }
        Ok(deterministic_i64(
            &self.workflow_id,
            &self.run_id,
            ordinal,
            min,
            max,
        ))
    }
}

/// One recorded event's projection: its determinism context and state delta.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectStep {
    /// Sequence number of the recorded event this step projects.
    pub seq: u64,
    /// Stable event-variant name for display.
    pub event_kind: &'static str,
    /// Correlation identity of the event, when it starts or carries one.
    pub correlation_key: Option<CorrelationKey>,
    /// Workflow-visible `now` at this step: the event's recorded timestamp.
    ///
    /// This is the authoritative determinism clock, never wall-clock time, and
    /// is exactly the value the production `now()` NIF serves for this step.
    pub now: DateTime<Utc>,
    /// The workflow-visible state delta this event contributes.
    pub projection: StepProjection,
}

/// The workflow-visible state delta one recorded event contributes.
#[derive(Clone, Debug, PartialEq)]
pub enum StepProjection {
    /// The run started with its type and input.
    Started {
        /// Workflow type recorded at start.
        workflow_type: String,
        /// Opaque input payload recorded at start.
        input: Payload,
    },
    /// A world-touching command resolved from recorded history to this outcome.
    Resolved(Resolution),
    /// The run reached a recorded terminal state.
    Terminal(ReplayTerminal),
    /// An asynchronous arrival event was recorded (not a command outcome row).
    AsyncArrival {
        /// Stable variant name of the asynchronous arrival event.
        kind: &'static str,
    },
    /// A recorded event that contributes no replay-visible state delta.
    NonReplay,
}

/// The divergent command at a non-determinism fault, expected vs found.
///
/// Built directly from the [`NonDeterminismError`] the resolver computes; the
/// expected/found shapes are the resolver's own, never recomputed here (C18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DivergentCommand {
    /// Sequence position of the recorded event at the mismatch.
    pub seq: u64,
    /// Shape of the command the workflow issued.
    pub expected: String,
    /// Shape of the recorded event found at the cursor position.
    pub found: String,
}

impl From<&NonDeterminismError> for DivergentCommand {
    fn from(error: &NonDeterminismError) -> Self {
        Self {
            seq: error.seq,
            expected: error.expected.clone(),
            found: error.found.clone(),
        }
    }
}

/// A mocked outcome substituted at the what-if fork point.
///
/// The caller chooses the outcome explicitly; there is no default (ADR-001,
/// CN2). Each variant carries the data the resulting [`Resolution`] needs.
#[derive(Clone, Debug, PartialEq)]
pub enum MockOutcome {
    /// Replace the activity at the fork with a successful completion.
    ActivityCompleted(Payload),
    /// Replace the activity at the fork with a terminal failure.
    ActivityFailed(ActivityError),
    /// Replace the child at the fork with a successful completion.
    ChildCompleted(Payload),
    /// Replace the child at the fork with a terminal failure.
    ChildFailed(WorkflowError),
    /// Replace the awaited signal at the fork with a delivered payload.
    SignalDelivered(Payload),
    /// Replace the timer at the fork with a firing.
    TimerFired,
}

/// The path a what-if re-run produces after the mocked fork point.
#[derive(Clone, Debug, PartialEq)]
pub enum WhatIfOutcome {
    /// The mocked resolution replaced the recorded one and the run resumed,
    /// projecting this resolution at the fork.
    Resolved {
        /// Sequence position of the recorded event that was forked.
        from_seq: u64,
        /// Resolution the mocked outcome produced at the fork.
        resolution: Resolution,
    },
    /// Driving the reconstructed command stream over the forked history reached
    /// a recorded terminal state with no live handoff.
    Terminal(ReplayTerminal),
    /// The forked command stream diverged from the forked history.
    Diverged(DivergentCommand),
}

/// Projects a complete inspection of `run_id` from a workflow's full history.
///
/// The history is sliced to the run's segment (reopen / continue-as-new aware),
/// then each recorded event is projected. World-touching events are resolved by
/// driving the real [`Replay`] over the command stream reconstructed from
/// history, so resolutions come from the production replay path. A
/// non-determinism fault is surfaced as [`RunInspection::divergence`] using the
/// resolver's own expected-vs-found shapes.
///
/// # Errors
///
/// Returns [`DurabilityError::HistoryShape`] when the history lacks a
/// `WorkflowStarted` for `run_id` or is otherwise malformed.
pub fn inspect_run(history: Vec<Event>, run_id: &RunId) -> Result<RunInspection, DurabilityError> {
    let segment = current_run_segment(history, run_id)?;
    if segment.is_empty() {
        return Err(empty_segment_error(run_id));
    }
    let workflow_id = run_workflow_id(&segment)?;
    let keys = correlation_keys_for_history(&segment);

    let mut replay = Replay::new(&workflow_id, run_id, segment.clone())?;
    let commands = reconstruct_commands(&segment);
    let mut command_index = 0;

    let mut steps = Vec::with_capacity(segment.len());

    for (event, correlation_key) in segment.iter().zip(keys) {
        let projection = match command_for_event(event) {
            CommandSlot::Issues => {
                let Some(command) = commands.get(command_index).cloned() else {
                    return Err(DurabilityError::HistoryShape {
                        reason: format!(
                            "reconstructed command stream is shorter than history at seq {}",
                            event.seq()
                        ),
                    });
                };
                command_index += 1;
                match replay.step(&command) {
                    Ok(ReplayStep::Recorded(resolution)) => StepProjection::Resolved(resolution),
                    Ok(ReplayStep::Terminal(terminal)) => StepProjection::Terminal(terminal),
                    // ResumeLive contributes no recorded delta. A live
                    // non-determinism fault is likewise not surfaced from the
                    // reconstructed stream: the resolver already recorded the
                    // authoritative expected-vs-found terminal, read back below
                    // (C18, CN5). Re-deriving it here would be a second,
                    // possibly divergent computation of the same fault.
                    Ok(ReplayStep::ResumeLive) | Err(DurabilityError::NonDeterminism(_)) => {
                        StepProjection::NonReplay
                    }
                    Err(other) => return Err(other),
                }
            }
            CommandSlot::Started {
                workflow_type,
                input,
            } => StepProjection::Started {
                workflow_type,
                input,
            },
            CommandSlot::Terminal => StepProjection::Terminal(terminal_projection(event)?),
            CommandSlot::AsyncArrival => StepProjection::AsyncArrival {
                kind: event_kind(event),
            },
            CommandSlot::NonReplay => StepProjection::NonReplay,
        };

        steps.push(InspectStep {
            seq: event.seq(),
            event_kind: event_kind(event),
            correlation_key,
            now: *event.recorded_at(),
            projection,
        });
    }

    // The recorded non-determinism terminal (a WorkflowFailed the resolver
    // produced via fail_on_violation) is the authoritative divergence: the
    // resolver already computed and recorded the expected-vs-found at the
    // sequence, so we read it back rather than recompute it (C18, CN5).
    let divergence = recorded_divergence(&segment);

    Ok(RunInspection {
        workflow_id: workflow_id.clone(),
        run_id: run_id.clone(),
        steps,
        random: RandomDrawProjection::new(workflow_id, run_id.clone()),
        divergence,
    })
}

/// Forks a what-if re-run from `from_seq` with a mocked outcome.
///
/// The recorded history is truncated at `from_seq` and the event at that
/// sequence is replaced by the event the mocked outcome implies; the
/// reconstructed command stream is then driven over the forked history through
/// the real [`Replay`]. The fork is entirely in memory and reads the source
/// history only — it never appends to the production event store or a live
/// recorder (invariant #3, CN5, ADR-007).
///
/// # Errors
///
/// Returns [`DurabilityError::HistoryShape`] when `from_seq` is not a
/// world-touching command outcome in the run segment, when the mocked outcome
/// does not match the family of the event at `from_seq`, or when the history is
/// malformed.
pub fn what_if_from(
    history: Vec<Event>,
    run_id: &RunId,
    from_seq: u64,
    mocked: &MockOutcome,
) -> Result<WhatIfOutcome, DurabilityError> {
    let segment = current_run_segment(history, run_id)?;
    let workflow_id = run_workflow_id(&segment)?;

    let fork_index = segment
        .iter()
        .position(|event| event.seq() == from_seq)
        .ok_or_else(|| DurabilityError::HistoryShape {
            reason: format!("run segment has no event at seq {from_seq} to fork from"),
        })?;

    let forked = forked_history(&segment, fork_index, &workflow_id, mocked)?;
    let mut replay = Replay::new(&workflow_id, run_id, forked.clone())?;
    let commands = reconstruct_commands(&forked);

    let mut last_resolution = None;
    for command in commands {
        match replay.step(&command) {
            Ok(ReplayStep::Recorded(resolution)) => last_resolution = Some(resolution),
            Ok(ReplayStep::Terminal(terminal)) => {
                return Ok(WhatIfOutcome::Terminal(terminal));
            }
            Ok(ReplayStep::ResumeLive) => break,
            Err(DurabilityError::NonDeterminism(error)) => {
                return Ok(WhatIfOutcome::Diverged(DivergentCommand::from(&error)));
            }
            Err(other) => return Err(other),
        }
    }

    match last_resolution {
        Some(resolution) => Ok(WhatIfOutcome::Resolved {
            from_seq,
            resolution,
        }),
        None => Err(DurabilityError::HistoryShape {
            reason: format!(
                "what-if from seq {from_seq} produced no resolution before history end"
            ),
        }),
    }
}

/// Classifies one recorded event for projection.
enum CommandSlot {
    /// The event is the outcome of a reconstructed world-touching command.
    Issues,
    /// The event started the run.
    Started {
        workflow_type: String,
        input: Payload,
    },
    /// The event is a recorded terminal lifecycle event.
    Terminal,
    /// The event is an asynchronous arrival (fire, delivery, child terminal).
    AsyncArrival,
    /// The event contributes no replay-visible state delta.
    NonReplay,
}

fn command_for_event(event: &Event) -> CommandSlot {
    match event {
        Event::WorkflowStarted {
            workflow_type,
            input,
            ..
        } => CommandSlot::Started {
            workflow_type: workflow_type.clone(),
            input: input.clone(),
        },
        // Command-issuing anchors: the resolver consumes their outcome events,
        // so the projection drives a reconstructed command for each anchor.
        Event::ActivityScheduled { .. }
        | Event::TimerStarted { .. }
        | Event::SignalReceived { .. }
        | Event::ChildWorkflowStarted { .. } => CommandSlot::Issues,
        Event::WorkflowCompleted { .. }
        | Event::WorkflowFailed { .. }
        | Event::WorkflowCancelled { .. }
        | Event::WorkflowTimedOut { .. }
        | Event::WorkflowContinuedAsNew { .. } => CommandSlot::Terminal,
        Event::TimerFired { .. }
        | Event::ActivityCompleted { .. }
        | Event::ActivityFailed { .. }
        | Event::ChildWorkflowCompleted { .. }
        | Event::ChildWorkflowFailed { .. } => CommandSlot::AsyncArrival,
        _ => CommandSlot::NonReplay,
    }
}

/// Reconstructs the world-touching command stream the engine fed the resolver.
///
/// The stream is derived from recorded anchor events in history order: each
/// activity schedule, timer start, signal receipt, and child spawn becomes the
/// command that produced it, with a child spawn followed by its await. The
/// terminal completion becomes a `CompleteWorkflow` command. This is exactly the
/// command stream replay consumes, so resolution comes from the production path.
fn reconstruct_commands(segment: &[Event]) -> Vec<Command> {
    let keys = correlation_keys_for_history(segment);
    let mut commands = Vec::new();

    for (event, key) in segment.iter().zip(keys) {
        match event {
            Event::ActivityScheduled {
                activity_type,
                input,
                ..
            } => {
                if let Some(key) = key {
                    commands.push(Command::RunActivity {
                        key,
                        activity_type: activity_type.clone(),
                        input: input.clone(),
                    });
                }
            }
            Event::TimerStarted { fire_at, .. } => {
                if let Some(key) = key {
                    commands.push(Command::StartTimer {
                        key,
                        fire_at: *fire_at,
                    });
                }
            }
            Event::SignalReceived { .. } => {
                if let Some(key) = key {
                    commands.push(Command::AwaitSignal { key });
                }
            }
            Event::ChildWorkflowStarted {
                child_workflow_id,
                workflow_type,
                input,
                ..
            } => {
                if let Some(key) = key {
                    commands.push(Command::SpawnChild {
                        key,
                        workflow_type: workflow_type.clone(),
                        input: input.clone(),
                    });
                    commands.push(Command::AwaitChild {
                        child_workflow_id: child_workflow_id.clone(),
                    });
                }
            }
            Event::WorkflowCompleted { result, .. } => {
                commands.push(Command::CompleteWorkflow {
                    result: result.clone(),
                });
            }
            _ => {}
        }
    }

    commands
}

/// Builds the forked history for a what-if: the segment up to and including the
/// fork point, with the outcome at the fork replaced by the mocked outcome.
fn forked_history(
    segment: &[Event],
    fork_index: usize,
    workflow_id: &WorkflowId,
    mocked: &MockOutcome,
) -> Result<Vec<Event>, DurabilityError> {
    let anchor = &segment[fork_index];
    let mocked_outcome = mocked_outcome_event(anchor, workflow_id, mocked)?;

    let mut forked: Vec<Event> = segment[..=fork_index].to_vec();
    forked.push(mocked_outcome);
    Ok(forked)
}

/// Produces the recorded outcome event a mocked outcome implies for one anchor.
fn mocked_outcome_event(
    anchor: &Event,
    workflow_id: &WorkflowId,
    mocked: &MockOutcome,
) -> Result<Event, DurabilityError> {
    let envelope = aion_core::EventEnvelope {
        seq: anchor.seq().saturating_add(1),
        recorded_at: *anchor.recorded_at(),
        workflow_id: workflow_id.clone(),
    };

    match (anchor, mocked) {
        (Event::ActivityScheduled { activity_id, .. }, MockOutcome::ActivityCompleted(result)) => {
            Ok(Event::ActivityCompleted {
                envelope,
                activity_id: activity_id.clone(),
                result: result.clone(),
                // NOI-0: the mocked completion resolves the scheduled activity's first (and only)
                // delivery — attempt 1 (one-based), matching the sibling `ActivityFailed` mock below.
                attempt: 1,
            })
        }
        (Event::ActivityScheduled { activity_id, .. }, MockOutcome::ActivityFailed(error)) => {
            ensure_terminal_activity_error(error)?;
            Ok(Event::ActivityFailed {
                envelope,
                activity_id: activity_id.clone(),
                error: error.clone(),
                attempt: 1,
            })
        }
        (Event::TimerStarted { timer_id, .. }, MockOutcome::TimerFired) => Ok(Event::TimerFired {
            envelope,
            timer_id: timer_id.clone(),
        }),
        (Event::SignalReceived { name, .. }, MockOutcome::SignalDelivered(payload)) => {
            Ok(Event::SignalReceived {
                envelope,
                name: name.clone(),
                payload: payload.clone(),
            })
        }
        (
            Event::ChildWorkflowStarted {
                child_workflow_id, ..
            },
            MockOutcome::ChildCompleted(result),
        ) => Ok(Event::ChildWorkflowCompleted {
            envelope,
            child_workflow_id: child_workflow_id.clone(),
            result: result.clone(),
        }),
        (
            Event::ChildWorkflowStarted {
                child_workflow_id, ..
            },
            MockOutcome::ChildFailed(error),
        ) => Ok(Event::ChildWorkflowFailed {
            envelope,
            child_workflow_id: child_workflow_id.clone(),
            error: error.clone(),
        }),
        (anchor, mocked) => Err(DurabilityError::HistoryShape {
            reason: format!(
                "mocked outcome {mocked:?} does not match the {} anchor at the fork",
                event_kind(anchor)
            ),
        }),
    }
}

fn ensure_terminal_activity_error(error: &ActivityError) -> Result<(), DurabilityError> {
    if error.is_retryable() {
        return Err(DurabilityError::HistoryShape {
            reason: "mocked activity failure must be terminal to resolve at the fork".to_owned(),
        });
    }
    Ok(())
}

fn terminal_projection(event: &Event) -> Result<ReplayTerminal, DurabilityError> {
    match event {
        Event::WorkflowCompleted { result, .. } => Ok(ReplayTerminal::Completed(result.clone())),
        Event::WorkflowFailed { error, .. } => Ok(ReplayTerminal::Failed(error.clone())),
        Event::WorkflowCancelled { reason, .. } => Ok(ReplayTerminal::Cancelled(reason.clone())),
        Event::WorkflowTimedOut { timeout, .. } => Ok(ReplayTerminal::TimedOut(timeout.clone())),
        Event::WorkflowContinuedAsNew { input, .. } => {
            Ok(ReplayTerminal::ContinuedAsNew(input.clone()))
        }
        other => Err(DurabilityError::HistoryShape {
            reason: format!(
                "terminal projection requested for non-terminal event {}",
                event_kind(other)
            ),
        }),
    }
}

/// Reads the divergent command back from a recorded non-determinism terminal.
///
/// When replay faults, the engine records a terminal `WorkflowFailed` whose
/// message is `fail_on_violation`'s formatting of the resolver's
/// [`NonDeterminismError`]. This reads that message back into a
/// [`DivergentCommand`] — the resolver's own expected-vs-found at the sequence,
/// never recomputed here (C18). Returns `None` when the segment holds no such
/// recorded fault, or when the message is not the recorder's own format.
fn recorded_divergence(segment: &[Event]) -> Option<DivergentCommand> {
    let message = segment.iter().find_map(|event| match event {
        Event::WorkflowFailed { error, .. }
            if error
                .message
                .starts_with(NON_DETERMINISM_WORKFLOW_ERROR_PREFIX) =>
        {
            Some((event.seq(), error.message.as_str()))
        }
        _ => None,
    })?;

    parse_recorded_divergence(message.0, message.1)
}

/// Parses the `expected`/`found`/`seq` fields out of a recorded fault message.
///
/// The message shape, fixed by `fail_on_violation` and [`NonDeterminismError`]'s
/// `Display`, is `"... at sequence {seq}: expected {expected}, found {found}"`.
/// Parsing is strict: an unexpected shape yields `None` rather than a guessed
/// value (no silent fabrication). `terminal_seq` is the failure event's own
/// sequence, used only as the fallback when the embedded sequence is unparsable.
fn parse_recorded_divergence(terminal_seq: u64, message: &str) -> Option<DivergentCommand> {
    let after_sequence = message.split_once(" at sequence ")?.1;
    let (seq_text, remainder) = after_sequence.split_once(": expected ")?;
    let (expected, found) = remainder.split_once(", found ")?;

    let seq = seq_text.trim().parse::<u64>().unwrap_or(terminal_seq);

    Some(DivergentCommand {
        seq,
        expected: expected.to_owned(),
        found: found.to_owned(),
    })
}

fn run_workflow_id(segment: &[Event]) -> Result<WorkflowId, DurabilityError> {
    segment
        .first()
        .map(|event| event.workflow_id().clone())
        .ok_or_else(|| DurabilityError::HistoryShape {
            reason: "run segment is empty".to_owned(),
        })
}

fn empty_segment_error(run_id: &RunId) -> DurabilityError {
    DurabilityError::HistoryShape {
        reason: format!("run segment for {run_id} is empty"),
    }
}

/// Stable event-variant name for display, mirroring the resolver's diagnostics.
fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::WorkflowStarted { .. } => "WorkflowStarted",
        Event::WorkflowCompleted { .. } => "WorkflowCompleted",
        Event::WorkflowFailed { .. } => "WorkflowFailed",
        Event::WorkflowCancelled { .. } => "WorkflowCancelled",
        Event::WorkflowTimedOut { .. } => "WorkflowTimedOut",
        Event::WorkflowContinuedAsNew { .. } => "WorkflowContinuedAsNew",
        Event::WorkflowReopened { .. } => "WorkflowReopened",
        Event::WorkflowPaused { .. } => "WorkflowPaused",
        Event::WorkflowResumed { .. } => "WorkflowResumed",
        Event::SearchAttributesUpdated { .. } => "SearchAttributesUpdated",
        Event::ActivityScheduled { .. } => "ActivityScheduled",
        Event::ActivityStarted { .. } => "ActivityStarted",
        Event::ActivityCompleted { .. } => "ActivityCompleted",
        Event::ActivityFailed { .. } => "ActivityFailed",
        Event::ActivityCancelled { .. } => "ActivityCancelled",
        Event::TimerStarted { .. } => "TimerStarted",
        Event::TimerFired { .. } => "TimerFired",
        Event::TimerCancelled { .. } => "TimerCancelled",
        Event::WithTimeoutCompleted { .. } => "WithTimeoutCompleted",
        Event::SignalReceived { .. } => "SignalReceived",
        Event::SignalSent { .. } => "SignalSent",
        Event::ChildWorkflowStarted { .. } => "ChildWorkflowStarted",
        Event::ChildWorkflowCompleted { .. } => "ChildWorkflowCompleted",
        Event::ChildWorkflowFailed { .. } => "ChildWorkflowFailed",
        Event::ChildWorkflowCancelled { .. } => "ChildWorkflowCancelled",
        Event::ScheduleCreated { .. } => "ScheduleCreated",
        Event::ScheduleUpdated { .. } => "ScheduleUpdated",
        Event::SchedulePaused { .. } => "SchedulePaused",
        Event::ScheduleResumed { .. } => "ScheduleResumed",
        Event::ScheduleDeleted { .. } => "ScheduleDeleted",
        Event::ScheduleTriggered { .. } => "ScheduleTriggered",
    }
}

#[cfg(test)]
#[path = "replay_inspect_tests.rs"]
mod replay_inspect_tests;
