//! ProcessContext-free resolution core for the two-phase `collect_*` NIFs.
//!
//! [`collect_step`] is one full collect resolution pass, invoked fresh on
//! every mailbox wake by the NIF shell in [`super::nif_concurrency`]. The
//! first live arrival allocates one contiguous activity-ordinal range, pins
//! [`PendingAwait::Collect`] **before any side effect** (re-entries must
//! reuse the pinned base — the ordinal counter advances on allocation, so an
//! unpinned re-entry would shear the ordinal↔event correlation), records
//! `ActivityScheduled`+`ActivityStarted` for the whole batch, and dispatches
//! all N through the shared completion-task machinery.
//!
//! Each pass runs a per-ordinal sweep — recorded terminal in the run
//! segment, else a takeable runtime-map completion (recorded now), else
//! missing. `collect_all`/`collect_map` fail fast on the **lowest-ordinal
//! recorded failure** (a set-derivable rule, so replay reproduces it from
//! the recorded terminals alone) and record `ActivityCancelled` for
//! everything unresolved; `collect_race` settles on the first recorded
//! terminal (batch ties break to the lowest ordinal) and cancels the rest.
//! An expired enclosing `with_timeout` scope cancels every unresolved member
//! and returns the canonical scope error.
//!
//! Replay never re-races and never consults the runtime maps: the result is
//! a total function of the per-ordinal recorded terminal set, read through a
//! direct run-segment scan (`resolve_command` is deliberately not used here
//! because its resolution path rejects `ActivityCancelled`).

use std::sync::Arc;

use aion_core::{ActivityError, ActivityErrorKind, ActivityId, Event, Payload};
use chrono::Utc;
use serde::Deserialize;

use crate::activity::bridge::ActivityDispatcher;
use crate::registry::Registry;
use crate::runtime::RuntimeHandle;
use crate::runtime::nif_activity_dispatch::{ActivityCall, spawn_completion_task};
use crate::runtime::nif_context::NifContext;
use crate::runtime::nif_state::{CollectKind, EngineNifState, PendingAwait};
use crate::runtime::nif_timeout::SCOPE_EXPIRED_MESSAGE;

/// One fan-out member, decoded from the SDK's activity-spec JSON.
#[derive(Deserialize)]
pub(super) struct ActivitySpec {
    name: String,
    input: String,
    config: String,
}

/// Engine seams one collect invocation resolves against.
pub(super) struct CollectDeps {
    pub(super) registry: Arc<Registry>,
    pub(super) runtime: Arc<RuntimeHandle>,
    pub(super) tokio_handle: tokio::runtime::Handle,
    pub(super) dispatcher: Option<Arc<dyn ActivityDispatcher>>,
}

/// One fan-out member's settlement state after a sweep.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OrdinalState {
    /// A recorded (or just-recorded) `ActivityCompleted` payload.
    Completed(String),
    /// A recorded (or just-recorded) terminal `ActivityFailed` message.
    Failed(String),
    /// A recorded `ActivityCancelled`.
    Cancelled,
    /// No terminal anywhere yet.
    Pending,
}

/// A settled race: the winning ordinal and its first-settle outcome
/// (`Ok` success payload or `Err` failure message).
type RaceSettlement = (u64, Result<String, String>);

/// Outcome of one ProcessContext-free collect resolution step.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CollectStep {
    /// A pending query's sentinel payload for `{error, <<"aion_query:...">>}`.
    QuerySentinel(String),
    /// Every member completed: payloads in input order (`all`/`map`).
    AllCompleted(Vec<String>),
    /// The race settled first-settle: the winner's success or failure.
    RaceWon(Result<String, String>),
    /// Fail-fast: the lowest-ordinal recorded failure's message.
    FailFast(String),
    /// The enclosing `with_timeout` scope expired (live or replayed).
    ScopeExpired(String),
    /// Park the calling process; a mailbox wake re-invokes the native.
    Suspend,
}

/// Two-phase collect resolution, invoked fresh on every wake.
///
/// Order is load-bearing: queries first (before any recorded-result fast
/// path), then the pin (allocate-once, before any side effect), then
/// scheduling/dispatch of any not-yet-recorded member, then the per-ordinal
/// sweep and the kind's settlement rule.
pub(super) fn collect_step(
    state: &EngineNifState,
    deps: &CollectDeps,
    pid: u64,
    kind: CollectKind,
    specs: &[ActivitySpec],
    label: &str,
) -> Result<CollectStep, String> {
    if let Some(sentinel) = super::nif_query_pump::take_pending_query_sentinel(state, pid) {
        return Ok(CollectStep::QuerySentinel(sentinel));
    }
    // The SDK never sends an empty list (concurrency.gleam guards), but an
    // empty fan-out resolves immediately and must pin nothing.
    if specs.is_empty() {
        return match kind {
            CollectKind::All => Ok(CollectStep::AllCompleted(Vec::new())),
            CollectKind::Race => Err("expected at least one activity".to_owned()),
        };
    }
    let count =
        u64::try_from(specs.len()).map_err(|_| "activity list length overflows u64".to_owned())?;
    let context = NifContext::new(
        pid,
        deps.registry.as_ref(),
        deps.tokio_handle.clone(),
        deps.runtime.signal_delivery(),
    )
    .map_err(|error| error.to_string())?;
    let base_ordinal = pin_or_allocate(state, &context, pid, kind, count)?;
    dispatch_unscheduled(deps, &context, specs, base_ordinal, label)?;
    match kind {
        CollectKind::All => settle_all(state, deps, &context, pid, base_ordinal, count),
        CollectKind::Race => settle_race(state, deps, &context, pid, base_ordinal, count),
    }
}

/// Reuse the pinned ordinal base, or allocate and pin one at first arrival.
fn pin_or_allocate(
    state: &EngineNifState,
    context: &NifContext,
    pid: u64,
    kind: CollectKind,
    count: u64,
) -> Result<u64, String> {
    match state.pending_awaits.get(&pid).map(|entry| entry.clone()) {
        Some(PendingAwait::Collect {
            base_ordinal,
            count: pinned_count,
            kind: pinned_kind,
        }) => {
            if pinned_count != count || pinned_kind != kind {
                return Err(format!(
                    "process is pinned to a different collect await \
                     (pinned {pinned_kind:?} of {pinned_count}, called {kind:?} of {count})"
                ));
            }
            Ok(base_ordinal)
        }
        Some(PendingAwait::Sleep { .. }) => {
            Err("process is pinned to a pending sleep await".to_owned())
        }
        Some(PendingAwait::Signal { .. }) => {
            Err("process is pinned to a pending signal await".to_owned())
        }
        Some(PendingAwait::Child { .. }) => {
            Err("process is pinned to a pending child await".to_owned())
        }
        None => {
            // First arrival in this engine epoch: allocate the contiguous
            // range exactly once and pin BEFORE any side effect, so a wake
            // re-entry can never re-allocate.
            let base_ordinal = context.allocate_activity_ordinals(count);
            state.pending_awaits.insert(
                pid,
                PendingAwait::Collect {
                    base_ordinal,
                    count,
                    kind,
                },
            );
            Ok(base_ordinal)
        }
    }
}

/// Record `Scheduled`+`Started` and dispatch every member the run segment
/// has no `ActivityScheduled` for; verify determinism at the anchor for the
/// members it does.
fn dispatch_unscheduled(
    deps: &CollectDeps,
    context: &NifContext,
    specs: &[ActivitySpec],
    base_ordinal: u64,
    label: &str,
) -> Result<(), String> {
    let mut fresh: Vec<(u64, &ActivitySpec)> = Vec::new();
    for (offset, spec) in specs.iter().enumerate() {
        let ordinal = base_ordinal + offset_to_u64(offset)?;
        match scheduled_activity_type(context.history(), ordinal) {
            Some(recorded) => {
                if recorded != spec.name {
                    return Err(format!(
                        "determinism violation: ordinal {ordinal} recorded activity type \
                         {recorded:?} but workflow code supplied {:?}",
                        spec.name
                    ));
                }
            }
            None => fresh.push((ordinal, spec)),
        }
    }
    if fresh.is_empty() {
        return Ok(());
    }
    let Some(dispatcher) = deps.dispatcher.as_ref() else {
        return Err(
            "no activity dispatcher configured — set one via EngineBuilder::activity_dispatcher"
                .to_owned(),
        );
    };
    // Record the whole batch first so the Scheduled range stays contiguous,
    // then dispatch; completions land in the runtime maps keyed by ordinal.
    for (ordinal, spec) in &fresh {
        let input = payload_from_json_text(&spec.input, label)?;
        context
            .record_activity_scheduled_started(
                Utc::now(),
                ActivityId::from_sequence_position(*ordinal),
                spec.name.clone(),
                input,
            )
            .map_err(|error| error.to_string())?;
    }
    for (ordinal, spec) in fresh {
        spawn_completion_task(
            &deps.tokio_handle,
            Arc::clone(&deps.runtime),
            Arc::clone(dispatcher),
            context.pid(),
            super::nif_activity::correlation_id(ordinal),
            ActivityCall {
                name: spec.name.clone(),
                input: spec.input.clone(),
                config: spec.config.clone(),
            },
        );
    }
    Ok(())
}

/// `collect_all`/`collect_map` settlement over one sweep of the range.
fn settle_all(
    state: &EngineNifState,
    deps: &CollectDeps,
    context: &NifContext,
    pid: u64,
    base_ordinal: u64,
    count: u64,
) -> Result<CollectStep, String> {
    let mut states = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for ordinal in base_ordinal..base_ordinal + count {
        let recorded = match recorded_terminal(context.history(), ordinal)? {
            Some(recorded) => recorded,
            None => take_and_record(deps, context, pid, ordinal)?,
        };
        states.push(recorded);
    }
    // Fail fast: the lowest-ordinal recorded failure. The rule is a function
    // of the recorded terminal *set*, so replay derives the same value.
    let lowest_failure = states.iter().find_map(|recorded| match recorded {
        OrdinalState::Failed(message) => Some(message.clone()),
        _ => None,
    });
    if let Some(message) = lowest_failure {
        cancel_pending(deps, context, pid, base_ordinal, &states)?;
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::FailFast(message));
    }
    if states
        .iter()
        .all(|recorded| matches!(recorded, OrdinalState::Completed(_)))
    {
        let results = states
            .into_iter()
            .filter_map(|recorded| match recorded {
                OrdinalState::Completed(payload) => Some(payload),
                _ => None,
            })
            .collect();
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::AllCompleted(results));
    }
    // An expired enclosing with_timeout deadline aborts the await: every
    // unresolved member is cancelled durably so replay derives the abort.
    //
    // The expiry decision is a pure function of the RESOLUTION snapshot
    // (`context.history()`), never a fresh store read: deciding the abort
    // from events newer than the snapshot this sweep settled on is the N-1
    // defect family. A deadline whose `TimerFired` landed after the
    // snapshot is settled by the wake it triggers, whose fresh snapshot
    // re-enters this sweep. No deadline-vs-terminal seq ordering is needed
    // on the recorded path (unlike await_child/receive_signal): member
    // terminals are recorded synchronously by this collect itself, and the
    // abort is recorded as the cancellation set, so replay reads the
    // decision instead of re-deriving the race.
    if super::nif_timeout::expired_scope_deadline(state, pid, context.history()).is_some() {
        cancel_pending(deps, context, pid, base_ordinal, &states)?;
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::ScopeExpired(SCOPE_EXPIRED_MESSAGE.to_owned()));
    }
    // No failure, not all completed, nothing pending: a replayed batch whose
    // live run was aborted by scope expiry (cancelled-without-failure).
    if !states
        .iter()
        .any(|recorded| matches!(recorded, OrdinalState::Pending))
    {
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::ScopeExpired(SCOPE_EXPIRED_MESSAGE.to_owned()));
    }
    Ok(CollectStep::Suspend)
}

/// `collect_race` settlement: first recorded terminal wins, batch ties
/// break to the lowest ordinal, losers are cancelled durably.
fn settle_race(
    state: &EngineNifState,
    deps: &CollectDeps,
    context: &NifContext,
    pid: u64,
    base_ordinal: u64,
    count: u64,
) -> Result<CollectStep, String> {
    let history = context.history();
    // The earliest-seq recorded non-cancelled terminal is the settled winner
    // (live: recorded by an earlier re-entry; replay: the only one).
    let mut winner = recorded_race_winner(history, base_ordinal, count)?;
    if winner.is_none() {
        // Take in ordinal order: of a batch sitting in the maps on one
        // wake, the lowest ordinal becomes the recorded winner.
        for ordinal in base_ordinal..base_ordinal + count {
            if recorded_terminal(history, ordinal)?.is_some() {
                // A recorded Cancelled never revives into a winner.
                continue;
            }
            match take_and_record(deps, context, pid, ordinal)? {
                OrdinalState::Completed(payload) => {
                    winner = Some((ordinal, Ok(payload)));
                    break;
                }
                OrdinalState::Failed(message) => {
                    winner = Some((ordinal, Err(message)));
                    break;
                }
                OrdinalState::Cancelled | OrdinalState::Pending => {}
            }
        }
    }
    if let Some((winner_ordinal, outcome)) = winner {
        for ordinal in base_ordinal..base_ordinal + count {
            if ordinal == winner_ordinal {
                continue;
            }
            drop_runtime_entries(deps, pid, ordinal);
            if recorded_terminal(history, ordinal)?.is_none() {
                record_cancelled(context, ordinal)?;
            }
        }
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::RaceWon(outcome));
    }
    // Snapshot-pure expiry, exactly as in `settle_all`: the abort is decided
    // from this resolution's history snapshot and recorded as the durable
    // cancellation set, so live and replay read the same decision. A
    // deadline firing after the snapshot re-enters via its wake. The
    // winner-first check order above is itself deterministic: a winner is a
    // recorded terminal, so replay settles it identically before consulting
    // the scope.
    if super::nif_timeout::expired_scope_deadline(state, pid, history).is_some() {
        for ordinal in base_ordinal..base_ordinal + count {
            drop_runtime_entries(deps, pid, ordinal);
            if recorded_terminal(history, ordinal)?.is_none() {
                record_cancelled(context, ordinal)?;
            }
        }
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::ScopeExpired(SCOPE_EXPIRED_MESSAGE.to_owned()));
    }
    // Every member cancelled with no winner: a replayed batch whose live
    // run was aborted by scope expiry before anything settled.
    let mut all_cancelled = true;
    for ordinal in base_ordinal..base_ordinal + count {
        if recorded_terminal(history, ordinal)? != Some(OrdinalState::Cancelled) {
            all_cancelled = false;
            break;
        }
    }
    if all_cancelled {
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::ScopeExpired(SCOPE_EXPIRED_MESSAGE.to_owned()));
    }
    Ok(CollectStep::Suspend)
}

/// Record `ActivityCancelled` for every pending member and drop any runtime
/// completion that raced in after the sweep.
fn cancel_pending(
    deps: &CollectDeps,
    context: &NifContext,
    pid: u64,
    base_ordinal: u64,
    states: &[OrdinalState],
) -> Result<(), String> {
    for (offset, recorded) in states.iter().enumerate() {
        if matches!(recorded, OrdinalState::Pending) {
            let ordinal = base_ordinal + offset_to_u64(offset)?;
            record_cancelled(context, ordinal)?;
            drop_runtime_entries(deps, pid, ordinal);
        }
    }
    Ok(())
}

/// Take this ordinal's runtime-map completion, if delivered, and record it.
fn take_and_record(
    deps: &CollectDeps,
    context: &NifContext,
    pid: u64,
    ordinal: u64,
) -> Result<OrdinalState, String> {
    let activity_id = ActivityId::from_sequence_position(ordinal);
    if let Some(payload) = deps.runtime.take_activity_result(pid, ordinal) {
        context
            .record_activity_completed(Utc::now(), activity_id, payload.clone())
            .map_err(|error| error.to_string())?;
        return Ok(OrdinalState::Completed(payload_text(&payload)?));
    }
    if let Some(error) = deps.runtime.take_activity_error(pid, ordinal) {
        context
            .record_activity_failed(Utc::now(), activity_id, terminal_error(&error.message), 1)
            .map_err(|inner| inner.to_string())?;
        return Ok(OrdinalState::Failed(error.message));
    }
    Ok(OrdinalState::Pending)
}

fn record_cancelled(context: &NifContext, ordinal: u64) -> Result<(), String> {
    context
        .record_activity_cancelled(Utc::now(), ActivityId::from_sequence_position(ordinal))
        .map_err(|error| error.to_string())
}

/// Drop both retained runtime-map entries for an ordinal (D5 hygiene at
/// settle time; the monitor drain covers post-exit stragglers).
fn drop_runtime_entries(deps: &CollectDeps, pid: u64, ordinal: u64) {
    drop(deps.runtime.take_activity_result(pid, ordinal));
    drop(deps.runtime.take_activity_error(pid, ordinal));
}

/// The recorded terminal for `ordinal` in this run's segment, if any.
fn recorded_terminal(history: &[Event], ordinal: u64) -> Result<Option<OrdinalState>, String> {
    let target = ActivityId::from_sequence_position(ordinal);
    for event in history {
        match event {
            Event::ActivityCompleted {
                activity_id,
                result,
                ..
            } if *activity_id == target => {
                return Ok(Some(OrdinalState::Completed(payload_text(result)?)));
            }
            Event::ActivityFailed {
                activity_id, error, ..
            } if *activity_id == target => {
                return Ok(Some(OrdinalState::Failed(error.message.clone())));
            }
            Event::ActivityCancelled { activity_id, .. } if *activity_id == target => {
                return Ok(Some(OrdinalState::Cancelled));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The earliest-seq recorded non-cancelled terminal in the fan-out range.
fn recorded_race_winner(
    history: &[Event],
    base_ordinal: u64,
    count: u64,
) -> Result<Option<RaceSettlement>, String> {
    let in_range = |activity_id: &ActivityId| {
        let position = activity_id.sequence_position();
        position >= base_ordinal && position < base_ordinal + count
    };
    for event in history {
        match event {
            Event::ActivityCompleted {
                activity_id,
                result,
                ..
            } if in_range(activity_id) => {
                return Ok(Some((
                    activity_id.sequence_position(),
                    Ok(payload_text(result)?),
                )));
            }
            Event::ActivityFailed {
                activity_id, error, ..
            } if in_range(activity_id) => {
                return Ok(Some((
                    activity_id.sequence_position(),
                    Err(error.message.clone()),
                )));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The recorded `ActivityScheduled` type for `ordinal`, if any.
fn scheduled_activity_type(history: &[Event], ordinal: u64) -> Option<String> {
    let target = ActivityId::from_sequence_position(ordinal);
    history.iter().find_map(|event| match event {
        Event::ActivityScheduled {
            activity_id,
            activity_type,
            ..
        } if *activity_id == target => Some(activity_type.clone()),
        _ => None,
    })
}

fn payload_from_json_text(text: &str, label: &str) -> Result<Payload, String> {
    let value = serde_json::from_str(text)
        .map_err(|error| format!("{label}: invalid JSON payload: {error}"))?;
    Payload::from_json(&value).map_err(|error| format!("{label}: {error}"))
}

fn payload_text(payload: &Payload) -> Result<String, String> {
    String::from_utf8(payload.bytes().to_vec())
        .map_err(|_| "recorded activity payload is not valid UTF-8".to_owned())
}

fn terminal_error(message: &str) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: message.to_owned(),
        details: None,
    }
}

fn offset_to_u64(offset: usize) -> Result<u64, String> {
    u64::try_from(offset).map_err(|_| "activity offset overflows u64".to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{
        ActivityError, ActivityErrorKind, ActivityId, ContentType, Event, EventEnvelope, Payload,
        RunId, WorkflowId, WorkflowStatus,
    };
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore, WriteToken};
    use serde_json::json;

    use super::{ActivitySpec, CollectDeps, CollectStep, collect_step};
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_state::{CollectKind, EngineNifState, PendingAwait};
    use crate::runtime::nif_timeout::TimeoutScope;
    use crate::runtime::{RuntimeConfig, RuntimeHandle};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Everything one `collect_step` test needs over a synthesized history.
    struct CollectHarness {
        state: Arc<EngineNifState>,
        deps: CollectDeps,
        store: Arc<dyn EventStore>,
        workflow_id: WorkflowId,
        handle: WorkflowHandle,
        pid: u64,
    }

    /// Seed `store` with `WorkflowStarted` + `events`, renumbered
    /// contiguously from seq 1, and return the minted identifiers.
    async fn seed_history(
        store: &Arc<dyn EventStore>,
        events: &[Event],
    ) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let mut seeded = vec![started_event(&workflow_id, &run_id)?];
        seeded.extend(events.iter().cloned());
        let mut sequenced = Vec::with_capacity(seeded.len());
        for (index, event) in seeded.into_iter().enumerate() {
            let seq = u64::try_from(index)? + 1;
            sequenced.push(reenvelope(event, &workflow_id, seq));
        }
        store
            .append(WriteToken::recorder(), &workflow_id, &sequenced, 0)
            .await?;
        Ok((workflow_id, run_id))
    }

    impl CollectHarness {
        /// Build over a fresh store seeded with `WorkflowStarted` + `events`.
        async fn over_events(events: &[Event]) -> Result<Self, Box<dyn std::error::Error>> {
            let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
            let (workflow_id, run_id) = seed_history(&store, events).await?;
            Self::over_store(store, workflow_id, run_id).await
        }

        /// Build a fresh engine epoch (registry, handle, ordinal counters)
        /// over an existing store — the unit-level analogue of an engine
        /// restart before replay.
        async fn over_store(
            store: Arc<dyn EventStore>,
            workflow_id: WorkflowId,
            run_id: RunId,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            let head = u64::try_from(store.read_history(&workflow_id).await?.len())?;
            let registry = Arc::new(Registry::default());
            let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
            let pid = runtime.spawn_test_process()?;
            let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), head);
            let handle = WorkflowHandle::new(WorkflowHandleParts {
                workflow_id: workflow_id.clone(),
                run_id: run_id.clone(),
                pid,
                workflow_type: "collect-parent".to_owned(),
                loaded_version: ContentHash::from_bytes([5; 32]),
                cached_status: WorkflowStatus::Running,
                residency: HandleResidency::Resident,
                recorder,
                completion: CompletionNotifier::new(),
            });
            registry.insert((workflow_id.clone(), run_id), handle.clone())?;
            let deps = CollectDeps {
                registry,
                runtime: Arc::clone(&runtime),
                tokio_handle: tokio::runtime::Handle::current(),
                dispatcher: None,
            };
            Ok(Self {
                state: Arc::new(EngineNifState::default()),
                deps,
                store,
                workflow_id,
                handle,
                pid,
            })
        }

        fn step(&self, kind: CollectKind, specs: &[ActivitySpec]) -> Result<CollectStep, String> {
            // Production runs this on a beamr scheduler thread with no
            // ambient Tokio context; block_in_place mirrors that so the
            // step's history reads can block_on the harness runtime.
            tokio::task::block_in_place(|| {
                collect_step(&self.state, &self.deps, self.pid, kind, specs, "collect")
            })
        }

        fn pinned(&self) -> Option<PendingAwait> {
            self.state.pending_awaits.get(&self.pid).map(|e| e.clone())
        }

        async fn cancelled_ordinals(&self) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
            Ok(self
                .store
                .read_history(&self.workflow_id)
                .await?
                .iter()
                .filter_map(|event| match event {
                    Event::ActivityCancelled { activity_id, .. } => {
                        Some(activity_id.sequence_position())
                    }
                    _ => None,
                })
                .collect())
        }

        fn shutdown(self) -> TestResult {
            self.deps.runtime.shutdown()?;
            Ok(())
        }
    }

    fn reenvelope(event: Event, workflow_id: &WorkflowId, seq: u64) -> Event {
        let envelope = EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        };
        match event {
            Event::WorkflowStarted {
                workflow_type,
                input,
                run_id,
                parent_run_id,
                ..
            } => Event::WorkflowStarted {
                envelope,
                workflow_type,
                input,
                run_id,
                parent_run_id,
            },
            Event::ActivityScheduled {
                activity_id,
                activity_type,
                input,
                ..
            } => Event::ActivityScheduled {
                envelope,
                activity_id,
                activity_type,
                input,
            },
            Event::ActivityStarted { activity_id, .. } => Event::ActivityStarted {
                envelope,
                activity_id,
            },
            Event::ActivityCompleted {
                activity_id,
                result,
                ..
            } => Event::ActivityCompleted {
                envelope,
                activity_id,
                result,
            },
            Event::ActivityFailed {
                activity_id,
                error,
                attempt,
                ..
            } => Event::ActivityFailed {
                envelope,
                activity_id,
                error,
                attempt,
            },
            Event::ActivityCancelled { activity_id, .. } => Event::ActivityCancelled {
                envelope,
                activity_id,
            },
            Event::TimerFired { timer_id, .. } => Event::TimerFired { envelope, timer_id },
            other => other,
        }
    }

    fn started_event(
        workflow_id: &WorkflowId,
        run_id: &RunId,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: chrono::Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: "collect-parent".to_owned(),
            input: Payload::from_json(&json!({ "fixture": "input" }))?,
            run_id: run_id.clone(),
            parent_run_id: None,
        })
    }

    fn placeholder_envelope() -> EventEnvelope {
        EventEnvelope {
            seq: 0,
            recorded_at: chrono::Utc::now(),
            workflow_id: WorkflowId::new_v4(),
        }
    }

    fn scheduled_started(ordinal: u64, name: &str) -> Vec<Event> {
        vec![
            Event::ActivityScheduled {
                envelope: placeholder_envelope(),
                activity_id: ActivityId::from_sequence_position(ordinal),
                activity_type: name.to_owned(),
                input: Payload::new(ContentType::Json, br#""in""#.to_vec()),
            },
            Event::ActivityStarted {
                envelope: placeholder_envelope(),
                activity_id: ActivityId::from_sequence_position(ordinal),
            },
        ]
    }

    fn completed(ordinal: u64, result: &str) -> Event {
        Event::ActivityCompleted {
            envelope: placeholder_envelope(),
            activity_id: ActivityId::from_sequence_position(ordinal),
            result: Payload::new(ContentType::Json, result.as_bytes().to_vec()),
        }
    }

    fn failed(ordinal: u64, message: &str) -> Event {
        Event::ActivityFailed {
            envelope: placeholder_envelope(),
            activity_id: ActivityId::from_sequence_position(ordinal),
            error: ActivityError {
                kind: ActivityErrorKind::Terminal,
                message: message.to_owned(),
                details: None,
            },
            attempt: 1,
        }
    }

    fn spec(name: &str) -> ActivitySpec {
        ActivitySpec {
            name: name.to_owned(),
            input: r#""in""#.to_owned(),
            config: "{}".to_owned(),
        }
    }

    fn specs(names: &[&str]) -> Vec<ActivitySpec> {
        names.iter().map(|name| spec(name)).collect()
    }

    fn scope_deadline_fired(ordinal: u64) -> Event {
        Event::TimerFired {
            envelope: placeholder_envelope(),
            timer_id: aion_core::TimerId::anonymous(ordinal),
        }
    }

    /// Arm the per-test timer bridge that backed the OLD fresh-read expiry
    /// path (`expired_scope_message` → `build_context_for_pid`); installing
    /// it proves the stale-snapshot tests fail if a fresh read is
    /// reintroduced, instead of accidentally passing because the fresh read
    /// was unavailable.
    fn install_fresh_read_bridge(harness: &CollectHarness) {
        crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
            &harness.state,
            Arc::clone(&harness.deps.registry),
            Arc::clone(&harness.store),
            tokio::runtime::Handle::current(),
            crate::runtime::SignalDeliveryConfig::default(),
        );
    }

    fn pending_batch(names: &[&str]) -> Vec<Event> {
        names
            .iter()
            .enumerate()
            .flat_map(|(ordinal, name)| {
                scheduled_started(u64::try_from(ordinal).unwrap_or(u64::MAX), name)
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pinned_base_is_reused_across_reentries_and_counter_advances_once() -> TestResult {
        let harness = CollectHarness::over_events(&pending_batch(&["alpha", "beta"])).await?;
        let two = specs(&["alpha", "beta"]);

        // First arrival: allocates the base once and pins it.
        assert_eq!(
            harness.step(CollectKind::All, &two),
            Ok(CollectStep::Suspend)
        );
        assert!(matches!(
            harness.pinned(),
            Some(PendingAwait::Collect {
                base_ordinal: 0,
                count: 2,
                kind: CollectKind::All,
            })
        ));
        assert_eq!(harness.handle.activity_ordinals_allocated(), 2);

        // Wake re-entry: the pinned base is reused, the counter must not
        // advance a second time.
        assert_eq!(
            harness.step(CollectKind::All, &two),
            Ok(CollectStep::Suspend)
        );
        assert!(matches!(
            harness.pinned(),
            Some(PendingAwait::Collect {
                base_ordinal: 0,
                ..
            })
        ));
        assert_eq!(harness.handle.activity_ordinals_allocated(), 2);
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fail_fast_returns_lowest_ordinal_failure_and_cancels_unresolved() -> TestResult {
        // Recorded failures at ordinals 1 and 2; ordinal 0 still pending.
        let mut events = pending_batch(&["a", "b", "c"]);
        events.push(failed(1, "boom-b"));
        events.push(failed(2, "boom-c"));
        let harness = CollectHarness::over_events(&events).await?;

        let step = harness.step(CollectKind::All, &specs(&["a", "b", "c"]));

        assert_eq!(step, Ok(CollectStep::FailFast("boom-b".to_owned())));
        assert_eq!(harness.cancelled_ordinals().await?, vec![0]);
        assert_eq!(harness.pinned(), None);
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_set_covers_exactly_the_unresolved_ordinals() -> TestResult {
        // 0 completed, 2 failed, 1 and 3 pending: the cancel set is {1, 3}.
        let mut events = pending_batch(&["a", "b", "c", "d"]);
        events.push(completed(0, r#""done-a""#));
        events.push(failed(2, "boom-c"));
        let harness = CollectHarness::over_events(&events).await?;

        let step = harness.step(CollectKind::All, &specs(&["a", "b", "c", "d"]));

        assert_eq!(step, Ok(CollectStep::FailFast("boom-c".to_owned())));
        assert_eq!(harness.cancelled_ordinals().await?, vec![1, 3]);
        assert_eq!(harness.pinned(), None);
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn race_winner_is_first_recorded_terminal_not_lowest_ordinal() -> TestResult {
        // Ordinal 1 settled first (its terminal is recorded); ordinal 0 is
        // still pending and must be cancelled, not preferred.
        let mut events = pending_batch(&["a", "b"]);
        events.push(completed(1, r#""win-b""#));
        let harness = CollectHarness::over_events(&events).await?;

        let step = harness.step(CollectKind::Race, &specs(&["a", "b"]));

        assert_eq!(step, Ok(CollectStep::RaceWon(Ok(r#""win-b""#.to_owned()))));
        assert_eq!(harness.cancelled_ordinals().await?, vec![0]);
        assert_eq!(harness.pinned(), None);

        // First-settle includes failure: a recorded failure wins the race.
        let mut events = pending_batch(&["a", "b"]);
        events.push(failed(1, "boom-b"));
        let failing = CollectHarness::over_events(&events).await?;
        assert_eq!(
            failing.step(CollectKind::Race, &specs(&["a", "b"])),
            Ok(CollectStep::RaceWon(Err("boom-b".to_owned())))
        );
        assert_eq!(failing.cancelled_ordinals().await?, vec![0]);
        harness.shutdown()?;
        failing.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn race_batch_tie_breaks_to_lowest_ordinal_and_drains_loser_entries() -> TestResult {
        let harness = CollectHarness::over_events(&pending_batch(&["a", "b"])).await?;
        // Both completions sit in the runtime maps on one wake.
        harness.deps.runtime.deliver_activity_completion_message(
            harness.pid,
            "activity:0",
            r#""r0""#.to_owned(),
        )?;
        harness.deps.runtime.deliver_activity_completion_message(
            harness.pid,
            "activity:1",
            r#""r1""#.to_owned(),
        )?;

        let step = harness.step(CollectKind::Race, &specs(&["a", "b"]));

        assert_eq!(step, Ok(CollectStep::RaceWon(Ok(r#""r0""#.to_owned()))));
        assert_eq!(harness.cancelled_ordinals().await?, vec![1]);
        // The loser's retained entry was dropped at settle (D5 hygiene).
        assert_eq!(harness.deps.runtime.retained_activity_completions(), 0);
        let history = harness.store.read_history(&harness.workflow_id).await?;
        let winner_terminals = history
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::ActivityCompleted { .. } | Event::ActivityFailed { .. }
                )
            })
            .count();
        assert_eq!(
            winner_terminals, 1,
            "exactly one non-cancelled terminal may exist: {history:#?}"
        );
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_list_resolves_immediately_without_pinning() -> TestResult {
        let harness = CollectHarness::over_events(&[]).await?;

        assert_eq!(
            harness.step(CollectKind::All, &[]),
            Ok(CollectStep::AllCompleted(Vec::new()))
        );
        assert_eq!(harness.pinned(), None);
        assert_eq!(harness.handle.activity_ordinals_allocated(), 0);

        let race = harness.step(CollectKind::Race, &[]);
        assert_eq!(race, Err("expected at least one activity".to_owned()));
        assert_eq!(harness.pinned(), None);
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expired_scope_cancels_unresolved_and_replay_derives_the_same_abort() -> TestResult {
        let mut events = pending_batch(&["a", "b"]);
        events.push(completed(0, r#""done-a""#));
        let harness = CollectHarness::over_events(&events).await?;
        harness
            .state
            .timeout_scopes
            .insert(9, TimeoutScope::replayed_for_test(harness.pid, true));
        harness
            .state
            .timeout_scope_stacks
            .insert(harness.pid, vec![9]);

        let step = harness.step(CollectKind::All, &specs(&["a", "b"]));

        assert_eq!(
            step,
            Ok(CollectStep::ScopeExpired(
                "timeout:deadline expired".to_owned()
            ))
        );
        assert_eq!(harness.cancelled_ordinals().await?, vec![1]);
        assert_eq!(harness.pinned(), None);
        let store = Arc::clone(&harness.store);
        let workflow_id = harness.workflow_id.clone();
        let run_id = harness.handle.run_id().clone();
        let history_len = store.read_history(&workflow_id).await?.len();
        harness.shutdown()?;

        // Fresh engine epoch over the same store (the restart analogue):
        // the recorded cancelled-without-failure set derives the same abort
        // and appends nothing.
        let replay = CollectHarness::over_store(store, workflow_id, run_id).await?;
        assert_eq!(
            replay.step(CollectKind::All, &specs(&["a", "b"])),
            Ok(CollectStep::ScopeExpired(
                "timeout:deadline expired".to_owned()
            ))
        );
        assert_eq!(
            replay.store.read_history(&replay.workflow_id).await?.len(),
            history_len,
            "replay must append nothing"
        );
        assert_eq!(replay.pinned(), None);
        replay.shutdown()
    }

    /// The live expiry decision must be a pure function of the RESOLUTION
    /// snapshot. Race modeled: the sweep's snapshot (a stale read) lacks the
    /// scope deadline's `TimerFired`, which is recorded by the time any
    /// later read runs. Before the fix `expired_scope_message` re-read the
    /// store, saw the fired deadline, and cancelled the pending member on
    /// the spot — an abort decided from events the resolution never
    /// observed. After the fix the stale-snapshot pass suspends; the
    /// deadline's wake re-enters with a fresh snapshot, cancels durably,
    /// and a fresh engine epoch derives the identical abort from the
    /// recorded set while appending nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn all_stale_snapshot_expiry_suspends_then_converges_with_replay() -> TestResult {
        let scope_timer = aion_core::TimerId::anonymous(7);
        // Stale snapshot = WorkflowStarted + batch (4) + Completed(0): the
        // deadline `TimerFired` (seq 7) is the one event past the window.
        let backing = Arc::new(crate::runtime::nif_test_stores::StaleReadStore::new(6));
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let mut events = pending_batch(&["a", "b"]);
        events.push(completed(0, r#""done-a""#));
        events.push(scope_deadline_fired(7));
        let (workflow_id, run_id) = seed_history(&store, &events).await?;
        let harness =
            CollectHarness::over_store(Arc::clone(&store), workflow_id.clone(), run_id.clone())
                .await?;
        install_fresh_read_bridge(&harness);
        backing.set_stale_target(&workflow_id, 1);
        // Live scope whose deadline is the recorded TimerFired(seq 7).
        harness
            .state
            .timeout_scopes
            .insert(21, TimeoutScope::live_for_test(harness.pid, scope_timer));
        harness
            .state
            .timeout_scope_stacks
            .insert(harness.pid, vec![21]);
        let two = specs(&["a", "b"]);

        // Pass 1 — stale resolution snapshot (no TimerFired): must suspend,
        // never decide the abort from a fresh read; nothing is cancelled.
        assert_eq!(
            harness.step(CollectKind::All, &two),
            Ok(CollectStep::Suspend),
            "a snapshot lacking the deadline must park, not branch"
        );
        assert_eq!(harness.cancelled_ordinals().await?, Vec::<u64>::new());

        // Pass 2 — fresh snapshot: the deadline is in the resolution read;
        // the unresolved member is cancelled durably and the await aborts.
        assert_eq!(
            harness.step(CollectKind::All, &two),
            Ok(CollectStep::ScopeExpired(
                "timeout:deadline expired".to_owned()
            ))
        );
        assert_eq!(harness.cancelled_ordinals().await?, vec![1]);
        assert_eq!(harness.pinned(), None);
        let history_len = store.read_history(&workflow_id).await?.len();
        harness.shutdown()?;

        // Fresh engine epoch over the final store (the restart analogue),
        // scope replay-derived expired exactly as `arm_scope` derives it:
        // the recorded cancellation set yields the same abort, appending
        // nothing.
        let replay = CollectHarness::over_store(store, workflow_id, run_id).await?;
        replay.state.timeout_scopes.insert(
            1,
            TimeoutScope::replayed_expired_with_deadline_for_test(
                replay.pid,
                aion_core::TimerId::anonymous(7),
            ),
        );
        replay
            .state
            .timeout_scope_stacks
            .insert(replay.pid, vec![1]);
        assert_eq!(
            replay.step(CollectKind::All, &two),
            Ok(CollectStep::ScopeExpired(
                "timeout:deadline expired".to_owned()
            )),
            "replay must take the same branch as the converged live run"
        );
        assert_eq!(
            replay.store.read_history(&replay.workflow_id).await?.len(),
            history_len,
            "replay must append nothing"
        );
        replay.shutdown()
    }

    /// `collect_race` twin of the stale-snapshot test: pre-fix, the fresh
    /// read aborted the race on the spot — cancelling every member and
    /// discarding the completion that was about to settle. Post-fix the
    /// stale pass parks, and the wake re-entry settles the delivered
    /// completion as the durably recorded winner — the branch a fresh
    /// engine epoch reproduces from the recorded terminals alone.
    #[tokio::test(flavor = "multi_thread")]
    async fn race_stale_snapshot_expiry_suspends_then_settles_the_recorded_winner() -> TestResult {
        let scope_timer = aion_core::TimerId::anonymous(7);
        // Stale snapshot = WorkflowStarted + batch (4): the deadline
        // `TimerFired` (seq 6) is the one event past the window.
        let backing = Arc::new(crate::runtime::nif_test_stores::StaleReadStore::new(5));
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let mut events = pending_batch(&["a", "b"]);
        events.push(scope_deadline_fired(7));
        let (workflow_id, run_id) = seed_history(&store, &events).await?;
        let harness =
            CollectHarness::over_store(Arc::clone(&store), workflow_id.clone(), run_id.clone())
                .await?;
        install_fresh_read_bridge(&harness);
        backing.set_stale_target(&workflow_id, 1);
        harness
            .state
            .timeout_scopes
            .insert(23, TimeoutScope::live_for_test(harness.pid, scope_timer));
        harness
            .state
            .timeout_scope_stacks
            .insert(harness.pid, vec![23]);
        let two = specs(&["a", "b"]);

        // Pass 1 — stale snapshot: park; pre-fix the fresh read cancelled
        // both members here and returned ScopeExpired.
        assert_eq!(
            harness.step(CollectKind::Race, &two),
            Ok(CollectStep::Suspend),
            "a snapshot lacking the deadline must park, not branch"
        );
        assert_eq!(harness.cancelled_ordinals().await?, Vec::<u64>::new());

        // The race window's other arrival: member 0's completion lands in
        // the runtime maps before the wake re-entry.
        harness.deps.runtime.deliver_activity_completion_message(
            harness.pid,
            "activity:0",
            r#""r0""#.to_owned(),
        )?;

        // Pass 2 — fresh snapshot: the completion is taken and recorded as
        // the winner (winner-first is deterministic — the recorded terminal
        // IS the decision), the loser is cancelled durably.
        assert_eq!(
            harness.step(CollectKind::Race, &two),
            Ok(CollectStep::RaceWon(Ok(r#""r0""#.to_owned())))
        );
        assert_eq!(harness.cancelled_ordinals().await?, vec![1]);
        assert_eq!(harness.pinned(), None);
        let history_len = store.read_history(&workflow_id).await?.len();
        harness.shutdown()?;

        // Fresh engine epoch, scope replay-derived expired: the recorded
        // winner settles the race identically, appending nothing.
        let replay = CollectHarness::over_store(store, workflow_id, run_id).await?;
        replay.state.timeout_scopes.insert(
            1,
            TimeoutScope::replayed_expired_with_deadline_for_test(
                replay.pid,
                aion_core::TimerId::anonymous(7),
            ),
        );
        replay
            .state
            .timeout_scope_stacks
            .insert(replay.pid, vec![1]);
        assert_eq!(
            replay.step(CollectKind::Race, &two),
            Ok(CollectStep::RaceWon(Ok(r#""r0""#.to_owned()))),
            "replay must settle the recorded winner, not re-derive the race"
        );
        assert_eq!(
            replay.store.read_history(&replay.workflow_id).await?.len(),
            history_len,
            "replay must append nothing"
        );
        replay.shutdown()
    }
}
