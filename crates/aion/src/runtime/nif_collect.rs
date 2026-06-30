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

use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use crate::durability::{FanOutCompletionResult, FanOutItem, FanOutOutcome};
use crate::registry::Registry;
use crate::runtime::RuntimeHandle;
use crate::runtime::nif_activity_dispatch::{FIRST_DELIVERY_ATTEMPT, spawn_completion_task};
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
    .map_err(|error| error.error_reason())?;
    let pin = pin_or_allocate(state, &context, pid, kind, count)?;
    dispatch_unscheduled(
        deps,
        &context,
        specs,
        pin.base_ordinal,
        pin.first_arrival,
        label,
    )?;
    match kind {
        CollectKind::All => settle_all(state, deps, &context, pid, pin.base_ordinal, count),
        CollectKind::Race => settle_race(state, deps, &context, pid, pin.base_ordinal, count),
    }
}

/// Pin result: the base ordinal and whether this is the first arrival in
/// this engine epoch (recovery or fresh run).
struct PinResult {
    base_ordinal: u64,
    first_arrival: bool,
}

/// Reuse the pinned ordinal base, or allocate and pin one at first arrival.
fn pin_or_allocate(
    state: &EngineNifState,
    context: &NifContext,
    pid: u64,
    kind: CollectKind,
    count: u64,
) -> Result<PinResult, String> {
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
            Ok(PinResult {
                base_ordinal,
                first_arrival: false,
            })
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
            Ok(PinResult {
                base_ordinal,
                first_arrival: true,
            })
        }
    }
}

/// Record `Scheduled`+`Started` and dispatch every member the run segment
/// has no `ActivityScheduled` for; verify determinism at the anchor for the
/// members it does.
///
/// On first arrival after recovery (`first_arrival` is true), activities
/// that ARE scheduled but lack a terminal event are re-dispatched without
/// re-recording — the original completion task died with the previous
/// engine process.
fn dispatch_unscheduled(
    deps: &CollectDeps,
    context: &NifContext,
    specs: &[ActivitySpec],
    base_ordinal: u64,
    first_arrival: bool,
    label: &str,
) -> Result<(), String> {
    let mut fresh: Vec<(u64, &ActivitySpec)> = Vec::new();
    let mut stale: Vec<(u64, &ActivitySpec)> = Vec::new();
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
                if first_arrival && recorded_terminal(context.history(), ordinal)?.is_none() {
                    stale.push((ordinal, spec));
                }
            }
            None => fresh.push((ordinal, spec)),
        }
    }
    if fresh.is_empty() && stale.is_empty() {
        return Ok(());
    }
    let Some(dispatcher) = deps.dispatcher.as_ref() else {
        if !fresh.is_empty() {
            return Err(
                "no activity dispatcher configured — set one via EngineBuilder::activity_dispatcher"
                    .to_owned(),
            );
        }
        return Ok(());
    };
    // With the outbox flag ON, fresh members route to the durable outbox: one
    // atomic `record_fan_out_dispatch` stages their Scheduled/Started events and
    // outbox rows, and the OutboxDispatcher (not a completion task) drives them.
    // Stale members keep today's path — their recovery is a later increment.
    // Flag OFF: today's behaviour exactly (per-item record + spawn for fresh∪stale).
    let outbox_enabled = deps.runtime.outbox_enabled();
    // The workflow's durable isolation namespace is the routing correctness boundary the staged
    // outbox rows must carry (NSTQ-2), so resolve it once and stamp it onto every fan-out item.
    let workflow_namespace = context.workflow_handle().namespace().to_owned();
    // #144: the queue the workflow was STARTED on, read once from recorded history, is each
    // member's fallback when neither the member override nor the workflow declared default selects
    // a queue — replacing the silent named-default fallback. Reading it from history (never live
    // state) keeps every member's resolved queue replay-deterministic.
    let start_time_task_queue = context.start_time_task_queue();
    let start_time_task_queue = start_time_task_queue.as_deref();
    if outbox_enabled {
        if !fresh.is_empty() {
            let items = fan_out_items(&fresh, &workflow_namespace, start_time_task_queue, label)?;
            context
                .record_fan_out_dispatch(Utc::now(), &items)
                .map_err(|error| error.error_reason())?;
        }
        // STALE recovery under the flag: the in-flight dispatch that died with the previous
        // engine process is re-staged by flipping each ordinal's durable outbox row back to
        // claimable `Pending`, so the OutboxDispatcher re-dispatches it. This replaces — does
        // not supplement — the in-process `spawn_completion_task` path (excluded below). The
        // completion dedup makes the redelivery at-least-once-safe.
        //
        // NSTQ-3 recovery: the re-staged row must re-target the SAME task queue the original
        // dispatch recorded, not the live default. Read it back from the recorded
        // `ActivityScheduled` event (the durable source of truth) so a recovered dispatch lands on
        // `(namespace, recorded_task_queue)`, never silently on `(namespace, "default")`.
        if !stale.is_empty() {
            let items =
                fan_out_items_recovered(&stale, &workflow_namespace, context.history(), label)?;
            context
                .rearm_outbox_pending(Utc::now(), &items)
                .map_err(|error| error.error_reason())?;
        }
    } else {
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
                    // NSTQ-4 (+#144): resolve once at this schedule seam, same
                    // precedence as the flag-ON fan-out and single-schedule paths
                    // (override > workflow default > start-time queue > default).
                    super::nif_activity::resolve_task_queue(&spec.config, start_time_task_queue),
                    // NODE-4: resolve the OPTIONAL node affinity at the same seam
                    // (member pin, else None), matching the live dispatch below.
                    super::nif_activity::resolve_node(&spec.config),
                )
                .map_err(|error| error.error_reason())?;
        }
    }
    let namespace = workflow_namespace;
    let workflow_id = context.workflow_id().clone();
    // Flag ON drives BOTH fresh (via record_fan_out_dispatch) and stale (via the
    // rearm_outbox_pending re-stage above) through the OutboxDispatcher, so spawn
    // neither here. Flag OFF spawns the whole fresh∪stale batch as today.
    let empty: &[(u64, &ActivitySpec)] = &[];
    let spawn_fresh: &[(u64, &ActivitySpec)] = if outbox_enabled { empty } else { &fresh };
    let spawn_stale: &[(u64, &ActivitySpec)] = if outbox_enabled { empty } else { &stale };
    for (ordinal, spec) in spawn_fresh.iter().chain(spawn_stale.iter()) {
        spawn_completion_task(
            &deps.tokio_handle,
            Arc::clone(&deps.runtime),
            Arc::clone(dispatcher),
            context.pid(),
            super::nif_activity::correlation_id(*ordinal),
            ActivityDispatch {
                namespace: namespace.clone(),
                // NSTQ-4 (+#144): resolve once at this schedule seam (member
                // override > workflow default > the workflow's recorded start-time
                // queue > the named default), matching the recorded
                // `ActivityScheduled` for this ordinal on the flag-OFF path.
                task_queue: super::nif_activity::resolve_task_queue(
                    &spec.config,
                    start_time_task_queue,
                ),
                // NODE-4: resolve the OPTIONAL node affinity at the same seam
                // (member pin, else None), matching the recorded
                // `ActivityScheduled` for this ordinal on the flag-OFF path.
                node: super::nif_activity::resolve_node(&spec.config),
                workflow_id: workflow_id.clone(),
                activity_id: ActivityId::from_sequence_position(*ordinal),
                name: spec.name.clone(),
                input: spec.input.clone(),
                config: spec.config.clone(),
                attempt: FIRST_DELIVERY_ATTEMPT,
                labels: super::nif_activity::labels_from_config(&spec.config),
            },
        );
    }
    Ok(())
}

/// Map a slice of `(ordinal, spec)` pairs to the durable-outbox [`FanOutItem`]s the recorder stages.
///
/// Drives the flag-ON fresh dispatch ([`NifContext::record_fan_out_dispatch`]); the stale re-arm
/// path ([`NifContext::rearm_outbox_pending`]) re-derives its items from recorded history instead
/// (see [`fan_out_items_recovered`]).
///
/// `start_time_task_queue` is the workflow's RECORDED start-time queue (#144), applied as each
/// member's fallback when its config selects no queue — read from history once by the caller so the
/// resolved queue replays deterministically.
fn fan_out_items(
    members: &[(u64, &ActivitySpec)],
    namespace: &str,
    start_time_task_queue: Option<&str>,
    label: &str,
) -> Result<Vec<FanOutItem>, String> {
    members
        .iter()
        .map(|(ordinal, spec)| {
            Ok(FanOutItem {
                ordinal: *ordinal,
                namespace: namespace.to_owned(),
                // NSTQ-4 (+#144): resolve each member's task queue once at this schedule seam
                // (member override > workflow declared default > the workflow's recorded start-time
                // queue > the named default) from its dispatch config, so the staged outbox row and
                // the recorded `ActivityScheduled` land on the chosen pool.
                task_queue: super::nif_activity::resolve_task_queue(
                    &spec.config,
                    start_time_task_queue,
                ),
                // NODE-4: resolve each member's OPTIONAL node affinity once at this schedule seam
                // (member pin, else None — no workflow default) from its dispatch config, so the
                // staged outbox row and the recorded `ActivityScheduled` land on the chosen node.
                node: super::nif_activity::resolve_node(&spec.config),
                activity_type: spec.name.clone(),
                input: payload_from_json_text(&spec.input, label)?,
            })
        })
        .collect()
}

/// Map a slice of recovered `(ordinal, spec)` pairs to [`FanOutItem`]s whose `task_queue` and
/// OPTIONAL `node` affinity are read back from each ordinal's recorded
/// [`Event::ActivityScheduled`] (NSTQ-3 / NODE-3 recovery).
///
/// Unlike [`fan_out_items`] — which resolves each member's task queue and OPTIONAL node from its
/// live dispatch config (NSTQ-4 / NODE-4) — this re-derives the durable task queue and node from
/// history so the re-staged outbox row re-targets the SAME pool AND node the crashed dispatch
/// chose. A history recorded before the `task_queue` field existed
/// (or any ordinal without a recorded `ActivityScheduled`) deterministically reads back the named
/// default queue via the event's serde default; a history recorded before the `node` field existed
/// deterministically reads back `None` (no affinity), so recovery is replay-safe.
fn fan_out_items_recovered(
    members: &[(u64, &ActivitySpec)],
    namespace: &str,
    history: &[Event],
    label: &str,
) -> Result<Vec<FanOutItem>, String> {
    members
        .iter()
        .map(|(ordinal, spec)| {
            Ok(FanOutItem {
                ordinal: *ordinal,
                namespace: namespace.to_owned(),
                task_queue: scheduled_task_queue(history, *ordinal)
                    .unwrap_or_else(|| String::from(aion_core::DEFAULT_TASK_QUEUE)),
                // NODE-3 recovery: re-derive the OPTIONAL node affinity from history so the
                // re-staged outbox row re-targets the SAME node the crashed dispatch chose. A
                // pre-field history reads back `None` (no affinity) deterministically.
                node: scheduled_node(history, *ordinal),
                activity_type: spec.name.clone(),
                input: payload_from_json_text(&spec.input, label)?,
            })
        })
        .collect()
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
    // Flag ON records terminals through the store-backed dedup primitive; the
    // returned OrdinalState is the same either way — the terminal for this
    // ordinal is in history whether this call Recorded it or found it Dropped.
    let outbox_enabled = deps.runtime.outbox_enabled();
    if let Some(payload) = deps.runtime.take_activity_result(pid, ordinal) {
        if outbox_enabled {
            let result = context
                .record_fan_out_completion(
                    Utc::now(),
                    ordinal,
                    FanOutOutcome::Completed(payload.clone()),
                )
                .map_err(|error| error.error_reason())?;
            log_unexpected_drop(result, ordinal);
        } else {
            context
                .record_activity_completed(Utc::now(), activity_id, payload.clone())
                .map_err(|error| error.error_reason())?;
        }
        return Ok(OrdinalState::Completed(payload_text(&payload)?));
    }
    if let Some(error) = deps.runtime.take_activity_error(pid, ordinal) {
        if outbox_enabled {
            let result = context
                .record_fan_out_completion(
                    Utc::now(),
                    ordinal,
                    FanOutOutcome::Failed {
                        error: terminal_error(&error.message),
                        attempt: 1,
                    },
                )
                .map_err(|inner| inner.error_reason())?;
            log_unexpected_drop(result, ordinal);
        } else {
            context
                .record_activity_failed(Utc::now(), activity_id, terminal_error(&error.message), 1)
                .map_err(|inner| inner.error_reason())?;
        }
        return Ok(OrdinalState::Failed(error.message));
    }
    Ok(OrdinalState::Pending)
}

/// Log a fan-out completion that the dedup primitive Dropped.
///
/// `settle_all`/`settle_race` short-circuit via `recorded_terminal` before
/// reaching `take_and_record`, so within one single-writer turn the result is
/// always `Recorded`. A `Dropped` here is unexpected on a single node — log it,
/// but the caller still maps to the correct terminal `OrdinalState` regardless.
fn log_unexpected_drop(result: FanOutCompletionResult, ordinal: u64) {
    if result == FanOutCompletionResult::Dropped {
        tracing::warn!(
            ordinal,
            "fan-out completion dropped as duplicate within a single-writer turn (unexpected single-node)"
        );
    }
}

fn record_cancelled(context: &NifContext, ordinal: u64) -> Result<(), String> {
    context
        .record_activity_cancelled_and_settle_outbox(Utc::now(), ordinal)
        .map_err(|error| error.error_reason())
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

/// The recorded `ActivityScheduled` task queue for `ordinal`, if any (NSTQ-3 recovery).
///
/// The durable source of truth for re-targeting the same pool on reopen/recovery. A history
/// recorded before the `task_queue` field existed decodes the field to the named default
/// (`aion_core::DEFAULT_TASK_QUEUE`) via the event's serde default, so this returns `"default"`
/// for such ordinals — never absent for a recorded `ActivityScheduled`.
fn scheduled_task_queue(history: &[Event], ordinal: u64) -> Option<String> {
    let target = ActivityId::from_sequence_position(ordinal);
    history.iter().find_map(|event| match event {
        Event::ActivityScheduled {
            activity_id,
            task_queue,
            ..
        } if *activity_id == target => Some(task_queue.clone()),
        _ => None,
    })
}

/// The recorded `ActivityScheduled` OPTIONAL node affinity for `ordinal` (NODE-3 recovery).
///
/// The durable source of truth for re-targeting the same node on reopen/recovery. Returns the
/// recorded `node` (`Some`/`None`) for a recorded `ActivityScheduled`, or `None` if no
/// `ActivityScheduled` exists for the ordinal. A history recorded before the `node` field existed
/// decodes the field to `None` via the event's serde default, so this is `None` (no affinity) for
/// such ordinals — never a sentinel, deterministically replay-safe.
fn scheduled_node(history: &[Event], ordinal: u64) -> Option<String> {
    let target = ActivityId::from_sequence_position(ordinal);
    history.iter().find_map(|event| match event {
        Event::ActivityScheduled {
            activity_id, node, ..
        } if *activity_id == target => node.clone(),
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

    use super::{
        ActivitySpec, CollectDeps, CollectStep, collect_step, fan_out_items,
        fan_out_items_recovered, scheduled_node, scheduled_task_queue,
    };
    use crate::activity::bridge::ActivityDispatch;
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_state::{CollectKind, EngineNifState, PendingAwait};
    use crate::runtime::nif_timeout::TimeoutScope;
    use crate::runtime::{RuntimeConfig, RuntimeHandle};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A dispatcher whose async dispatch never resolves: the schedule path
    /// records `ActivityScheduled`+`ActivityStarted` durably, spawns the
    /// completion task, then parks — so the recorded events are observable
    /// without a completion racing the assertion.
    struct NeverDispatcher;

    impl crate::activity::bridge::ActivityDispatcher for NeverDispatcher {
        fn dispatch(&self, _request: ActivityDispatch) -> Result<String, String> {
            // Never invoked: dispatch_async is overridden to a pending future.
            Err("NeverDispatcher: dispatch_async never resolves".to_owned())
        }

        fn dispatch_async(
            self: Arc<Self>,
            _request: ActivityDispatch,
        ) -> futures::future::BoxFuture<'static, Result<String, String>> {
            Box::pin(std::future::pending())
        }
    }

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
                namespace: String::from("default"),
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

        /// Every recorded `ActivityScheduled` as `(ordinal, task_queue)`, in
        /// history order — the durable record the fresh dispatch stamped.
        async fn scheduled_task_queues(
            &self,
        ) -> Result<Vec<(u64, String)>, Box<dyn std::error::Error>> {
            Ok(self
                .store
                .read_history(&self.workflow_id)
                .await?
                .iter()
                .filter_map(|event| match event {
                    Event::ActivityScheduled {
                        activity_id,
                        task_queue,
                        ..
                    } => Some((activity_id.sequence_position(), task_queue.clone())),
                    _ => None,
                })
                .collect())
        }

        /// Every recorded `ActivityScheduled` as `(ordinal, node)`, in history
        /// order — the durable OPTIONAL affinity the fresh dispatch stamped.
        async fn scheduled_nodes(
            &self,
        ) -> Result<Vec<(u64, Option<String>)>, Box<dyn std::error::Error>> {
            Ok(self
                .store
                .read_history(&self.workflow_id)
                .await?
                .iter()
                .filter_map(|event| match event {
                    Event::ActivityScheduled {
                        activity_id, node, ..
                    } => Some((activity_id.sequence_position(), node.clone())),
                    _ => None,
                })
                .collect())
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
                package_version,
                ..
            } => Event::WorkflowStarted {
                envelope,
                workflow_type,
                input,
                run_id,
                parent_run_id,
                package_version,
            },
            Event::ActivityScheduled {
                activity_id,
                activity_type,
                input,
                task_queue,
                node,
                ..
            } => Event::ActivityScheduled {
                envelope,
                activity_id,
                activity_type,
                input,
                task_queue,
                node,
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
            Event::SearchAttributesUpdated {
                workflow_id: attribute_workflow_id,
                attributes,
                ..
            } => Event::SearchAttributesUpdated {
                envelope,
                workflow_id: attribute_workflow_id,
                attributes,
            },
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
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
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
                task_queue: String::from("default"),
                node: None,
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

    fn scheduled_started_on(
        ordinal: u64,
        name: &str,
        task_queue: &str,
        node: Option<&str>,
    ) -> Vec<Event> {
        vec![
            Event::ActivityScheduled {
                envelope: placeholder_envelope(),
                activity_id: ActivityId::from_sequence_position(ordinal),
                activity_type: name.to_owned(),
                input: Payload::new(ContentType::Json, br#""in""#.to_vec()),
                task_queue: task_queue.to_owned(),
                node: node.map(str::to_owned),
            },
            Event::ActivityStarted {
                envelope: placeholder_envelope(),
                activity_id: ActivityId::from_sequence_position(ordinal),
            },
        ]
    }

    /// NSTQ-3 recovery: when an in-flight dispatch is re-staged from history after a restart, the
    /// re-armed item must re-target the SAME task queue the original `ActivityScheduled` recorded
    /// (`(namespace, X)`), not silently fall back to `(namespace, "default")`. This is the durable
    /// source-of-truth path the stale-recovery branch of `dispatch_unscheduled` consumes.
    #[test]
    fn recovery_re_targets_the_recorded_task_queue_not_default() -> TestResult {
        // History recorded the activity on task queue "claude" before the crash.
        let history = scheduled_started_on(0, "work", "claude", None);
        assert_eq!(scheduled_task_queue(&history, 0).as_deref(), Some("claude"));

        let work = spec("work");
        let members = [(0u64, &work)];
        let items = fan_out_items_recovered(&members, "remote", &history, "recovery")
            .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].namespace, "remote",
            "recovery keeps the workflow namespace"
        );
        assert_eq!(
            items[0].task_queue, "claude",
            "recovery must re-target the RECORDED task queue, never the default"
        );
        Ok(())
    }

    /// NSTQ-3 recovery replay-safety: an OLD history (recorded before the `task_queue` field
    /// existed) decodes its `ActivityScheduled` `task_queue` to the named default, so a recovered
    /// dispatch deterministically re-targets `(namespace, "default")` — never panics, never differs.
    #[test]
    fn recovery_from_pre_field_history_defaults_task_queue() -> TestResult {
        // Reconstruct the exact old wire form: serialize a current event, strip task_queue, decode.
        let current = &scheduled_started_on(0, "work", "ignored-when-stripped", None)[0];
        let mut value = serde_json::to_value(current)?;
        let data = value
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("ActivityScheduled must serialize to a tagged object with a `data` map")?;
        assert!(data.remove("task_queue").is_some());
        let old_event: Event = serde_json::from_value(value)?;
        let history = vec![old_event];

        let work = spec("work");
        let members = [(0u64, &work)];
        let items = fan_out_items_recovered(&members, "remote", &history, "recovery")
            .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

        assert_eq!(
            items[0].task_queue, "default",
            "an old history with no recorded task_queue must recover as the named default"
        );
        Ok(())
    }

    /// NODE-3 recovery: when an in-flight dispatch is re-staged from history after a restart, the
    /// re-armed item must re-target the SAME node the original `ActivityScheduled` recorded, not
    /// silently drop the affinity. This is the durable source-of-truth path the stale-recovery
    /// branch of `dispatch_unscheduled` consumes.
    #[test]
    fn recovery_re_targets_the_recorded_node_not_none() -> TestResult {
        // History recorded the activity pinned to node "box-7" before the crash.
        let history = scheduled_started_on(0, "work", "claude", Some("box-7"));
        assert_eq!(scheduled_node(&history, 0).as_deref(), Some("box-7"));

        let work = spec("work");
        let members = [(0u64, &work)];
        let items = fan_out_items_recovered(&members, "remote", &history, "recovery")
            .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].node.as_deref(),
            Some("box-7"),
            "recovery must re-target the RECORDED node, never silently drop affinity"
        );
        Ok(())
    }

    /// NODE-3 recovery replay-safety: an OLD history (recorded before the `node` field existed)
    /// decodes its `ActivityScheduled` `node` to `None`, so a recovered dispatch deterministically
    /// re-stages with no affinity — never a sentinel, never panics, never differs.
    #[test]
    fn recovery_from_pre_field_history_has_no_node() -> TestResult {
        // Reconstruct the exact old wire form: serialize a current event, strip node, decode.
        let current = &scheduled_started_on(0, "work", "claude", Some("ignored-when-stripped"))[0];
        let mut value = serde_json::to_value(current)?;
        let data = value
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("ActivityScheduled must serialize to a tagged object with a `data` map")?;
        assert!(data.remove("node").is_some());
        let old_event: Event = serde_json::from_value(value)?;
        let history = vec![old_event];
        assert_eq!(scheduled_node(&history, 0), None);

        let work = spec("work");
        let members = [(0u64, &work)];
        let items = fan_out_items_recovered(&members, "remote", &history, "recovery")
            .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

        assert_eq!(
            items[0].node, None,
            "an old history with no recorded node must recover as no affinity (None)"
        );
        Ok(())
    }

    /// NSTQ-4 fresh dispatch: a fan-out member whose config selects task queue
    /// "claude" produces an outbox item (the durable row) on "claude", not the
    /// named default. This is the host-decode → outbox-row seam.
    #[test]
    fn fresh_fan_out_item_carries_the_selected_task_queue() -> TestResult {
        let claude = spec_with_task_queue("work", Some("claude"), None);
        let members = [(0u64, &claude)];
        let items = fan_out_items(&members, "remote", None, "fanout")
            .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].namespace, "remote");
        assert_eq!(
            items[0].task_queue, "claude",
            "the member override must reach the fresh outbox row"
        );
        Ok(())
    }

    /// NSTQ-4 precedence at the fresh-dispatch seam: a member with no override
    /// under a workflow defaulting to "gpu" resolves to "gpu"; with neither (and
    /// no start-time queue), to the named default. Mixed members each land on
    /// their own resolved queue.
    #[test]
    fn fresh_fan_out_items_resolve_precedence_per_member() -> TestResult {
        let overridden = spec_with_task_queue("a", Some("claude"), Some("gpu"));
        let defaulted = spec_with_task_queue("b", None, Some("gpu"));
        let plain = spec_with_task_queue("c", None, None);
        let members = [(0u64, &overridden), (1u64, &defaulted), (2u64, &plain)];
        let items = fan_out_items(&members, "remote", None, "fanout")
            .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

        let queues: Vec<&str> = items.iter().map(|i| i.task_queue.as_str()).collect();
        assert_eq!(
            queues,
            vec!["claude", "gpu", aion_core::DEFAULT_TASK_QUEUE],
            "override > workflow default > the named default, resolved once per member"
        );
        Ok(())
    }

    /// #144 precedence at the fresh-dispatch seam: under a workflow STARTED on
    /// "started-on", a member with an explicit override keeps it, a member with
    /// only the SDK-declared workflow default keeps that, and a member that
    /// selects neither falls back to the workflow's start-time queue — NOT the
    /// named default. The start-time queue threads in once for the whole batch.
    #[test]
    fn fresh_fan_out_items_fall_back_to_the_start_time_queue() -> TestResult {
        let overridden = spec_with_task_queue("a", Some("claude"), None);
        let defaulted = spec_with_task_queue("b", None, Some("gpu"));
        let plain = spec_with_task_queue("c", None, None);
        let members = [(0u64, &overridden), (1u64, &defaulted), (2u64, &plain)];
        let items = fan_out_items(&members, "remote", Some("started-on"), "fanout")
            .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

        let queues: Vec<&str> = items.iter().map(|i| i.task_queue.as_str()).collect();
        assert_eq!(
            queues,
            vec!["claude", "gpu", "started-on"],
            "override > workflow default > the recorded start-time queue (never the named default)"
        );
        Ok(())
    }

    /// NODE-4 fresh dispatch: a fan-out member whose config pins node "box-7"
    /// produces an outbox item (the durable row) carrying node=Some("box-7"); a
    /// member with no pin carries node=None. This is the host-decode → outbox-row
    /// seam for the OPTIONAL affinity.
    #[test]
    fn fresh_fan_out_item_carries_the_selected_node() -> TestResult {
        let pinned = spec_with_node("a", Some("box-7"));
        let unpinned = spec_with_node("b", None);
        let members = [(0u64, &pinned), (1u64, &unpinned)];
        let items = fan_out_items(&members, "remote", None, "fanout")
            .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].node.as_deref(),
            Some("box-7"),
            "the member pin must reach the fresh outbox row"
        );
        assert_eq!(
            items[1].node, None,
            "an unpinned member must carry no affinity"
        );
        Ok(())
    }

    /// NODE-4 end-to-end through the flag-OFF schedule path: scheduling a batch
    /// mixing a node-pinned member and an unpinned member records each
    /// `ActivityScheduled` with its resolved node (host decode → recorder →
    /// durable history).
    #[tokio::test(flavor = "multi_thread")]
    async fn scheduled_events_record_each_members_resolved_node() -> TestResult {
        let mut harness = CollectHarness::over_events(&[]).await?;
        harness.deps.dispatcher = Some(Arc::new(NeverDispatcher));
        let specs = vec![
            spec_with_node("a", Some("box-7")),
            spec_with_node("b", None),
        ];

        assert_eq!(
            harness.step(CollectKind::All, &specs),
            Ok(CollectStep::Suspend)
        );

        assert_eq!(
            harness.scheduled_nodes().await?,
            vec![(0, Some("box-7".to_owned())), (1, None)],
            "each recorded ActivityScheduled must carry its resolved node affinity"
        );
        harness.shutdown()
    }

    /// NSTQ-4 end-to-end through the flag-OFF schedule path: scheduling a batch
    /// that mixes a "claude"-selected member with a workflow-"gpu"-defaulted
    /// member and a no-selection member records each `ActivityScheduled` on its
    /// own resolved task queue (host decode → recorder → durable history).
    #[tokio::test(flavor = "multi_thread")]
    async fn scheduled_events_record_each_members_resolved_task_queue() -> TestResult {
        let mut harness = CollectHarness::over_events(&[]).await?;
        // A dispatcher that never completes: the fresh batch's Scheduled+Started
        // events are recorded durably up-front, then the step parks at Suspend
        // (no completion arrives), so the recorded task queues are observable
        // without racing a settlement.
        harness.deps.dispatcher = Some(Arc::new(NeverDispatcher));
        let specs = vec![
            spec_with_task_queue("a", Some("claude"), Some("gpu")),
            spec_with_task_queue("b", None, Some("gpu")),
            spec_with_task_queue("c", None, None),
        ];

        assert_eq!(
            harness.step(CollectKind::All, &specs),
            Ok(CollectStep::Suspend)
        );

        assert_eq!(
            harness.scheduled_task_queues().await?,
            vec![
                (0, "claude".to_owned()),
                (1, "gpu".to_owned()),
                (2, aion_core::DEFAULT_TASK_QUEUE.to_owned()),
            ],
            "each recorded ActivityScheduled must carry its resolved task queue"
        );
        harness.shutdown()
    }

    /// A `SearchAttributesUpdated` event recording the workflow's start-time
    /// task queue as the `aion.task_queue` attribute — exactly as the server
    /// stamps it in the same append as `WorkflowStarted` (#144). This is the
    /// durable, history-resident source the start-time-queue fallback reads.
    fn start_time_task_queue_event(queue: &str) -> Event {
        Event::SearchAttributesUpdated {
            envelope: placeholder_envelope(),
            workflow_id: WorkflowId::new_v4(),
            attributes: std::collections::HashMap::from([(
                aion_core::START_TIME_TASK_QUEUE_ATTRIBUTE.to_owned(),
                aion_core::SearchAttributeValue::String(queue.to_owned()),
            )]),
        }
    }

    /// #144 end-to-end through the flag-OFF schedule path: a workflow STARTED on
    /// "started-on" (recorded as the `aion.task_queue` search attribute) that
    /// fans out a member selecting NO task queue anywhere records its
    /// `ActivityScheduled` on "started-on", NOT the named default — the
    /// previously-silent fallback. The start-time queue is read from recorded
    /// history, so it is replay-stable.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_selection_records_on_the_workflow_start_time_queue() -> TestResult {
        let harness =
            CollectHarness::over_events(&[start_time_task_queue_event("started-on")]).await?;
        let mut harness = harness;
        harness.deps.dispatcher = Some(Arc::new(NeverDispatcher));
        // One member with no override and no workflow declared default.
        let specs = vec![spec_with_task_queue("a", None, None)];

        assert_eq!(
            harness.step(CollectKind::All, &specs),
            Ok(CollectStep::Suspend)
        );

        assert_eq!(
            harness.scheduled_task_queues().await?,
            vec![(0, "started-on".to_owned())],
            "a no-selection activity must record on the workflow's start-time queue, not default"
        );
        harness.shutdown()
    }

    /// #144 replay-stability: recovery (a fresh engine epoch over the same store)
    /// re-resolves a no-selection activity to the SAME recorded start-time queue.
    /// The first epoch schedules the activity on "started-on" and parks; a fresh
    /// epoch replays the same collect over the recorded history and the recorded
    /// `ActivityScheduled` still reads back "started-on" — never re-defaulting,
    /// never diverging run-to-run. Mirrors `recovery_re_targets_the_recorded_*`.
    #[tokio::test(flavor = "multi_thread")]
    async fn recovery_re_resolves_the_start_time_queue_not_default() -> TestResult {
        // Epoch 1: schedule the no-selection member on the start-time queue.
        let first =
            CollectHarness::over_events(&[start_time_task_queue_event("started-on")]).await?;
        let mut first = first;
        first.deps.dispatcher = Some(Arc::new(NeverDispatcher));
        let specs = vec![spec_with_task_queue("a", None, None)];
        assert_eq!(
            first.step(CollectKind::All, &specs),
            Ok(CollectStep::Suspend)
        );
        let recorded_first = first.scheduled_task_queues().await?;
        assert_eq!(recorded_first, vec![(0, "started-on".to_owned())]);
        let store = Arc::clone(&first.store);
        let workflow_id = first.workflow_id.clone();
        let run_id = first.handle.run_id().clone();
        first.shutdown()?;

        // Epoch 2 (the restart analogue): a fresh registry/runtime/ordinal
        // counter over the SAME store replays the recorded history.
        let replay = CollectHarness::over_store(store, workflow_id, run_id).await?;
        let mut replay = replay;
        replay.deps.dispatcher = Some(Arc::new(NeverDispatcher));
        assert_eq!(
            replay.step(CollectKind::All, &specs),
            Ok(CollectStep::Suspend),
            "replay must re-enter the same pending collect"
        );
        assert_eq!(
            replay.scheduled_task_queues().await?,
            vec![(0, "started-on".to_owned())],
            "replay must re-resolve to the recorded start-time queue, never the default"
        );
        replay.shutdown()
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

    /// An [`ActivitySpec`] carrying the SDK's two task-queue selection fields in
    /// its dispatch config: `task_queue` (the per-activity override) and
    /// `workflow_task_queue` (the workflow-level default). `None` encodes the
    /// SDK's "no selection" as JSON null.
    fn spec_with_task_queue(
        name: &str,
        task_queue: Option<&str>,
        workflow_task_queue: Option<&str>,
    ) -> ActivitySpec {
        let field = |value: Option<&str>| match value {
            Some(text) => format!("\"{text}\""),
            None => "null".to_owned(),
        };
        ActivitySpec {
            name: name.to_owned(),
            input: r#""in""#.to_owned(),
            config: format!(
                r#"{{"labels":{{}},"task_queue":{},"workflow_task_queue":{}}}"#,
                field(task_queue),
                field(workflow_task_queue)
            ),
        }
    }

    /// An [`ActivitySpec`] carrying the SDK's OPTIONAL `node` affinity field in
    /// its dispatch config. `None` encodes the SDK's "no pin" as JSON null.
    fn spec_with_node(name: &str, node: Option<&str>) -> ActivitySpec {
        let field = match node {
            Some(text) => format!("\"{text}\""),
            None => "null".to_owned(),
        };
        ActivitySpec {
            name: name.to_owned(),
            input: r#""in""#.to_owned(),
            config: format!(
                r#"{{"labels":{{}},"task_queue":null,"workflow_task_queue":null,"node":{field}}}"#
            ),
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
