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

use aion_core::{ActivityId, Event};
use chrono::Utc;
use serde::Deserialize;

use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use crate::durability::FanOutItem;
use crate::error::EngineError;
use crate::registry::Registry;
use crate::runtime::RuntimeHandle;
use crate::runtime::nif_activity_dispatch::{FIRST_DELIVERY_ATTEMPT, spawn_completion_task};
use crate::runtime::nif_context::NifContext;
use crate::runtime::nif_state::{CollectKind, EngineNifState, PendingAwait};

/// One fan-out member, decoded from the SDK's activity-spec JSON.
#[derive(Deserialize)]
pub(super) struct ActivitySpec {
    name: String,
    input: String,
    config: String,
}

impl ActivitySpec {
    /// Defense twin of the arity-3 dispatch wire's tier check: fan-out members
    /// are dispatched remotely, so a member selecting the in-VM tier must be
    /// refused at decode time (CUT 3 scopes in-VM to the single-dispatch
    /// arity-4 wire; fan-out carries no runner thunks).
    pub(super) fn selects_in_vm(&self) -> bool {
        super::nif_activity::config_tier(&self.config).as_deref()
            == Some(super::nif_activity::IN_VM_TIER)
    }

    /// Activity name for decode-time diagnostics.
    pub(super) fn spec_name(&self) -> &str {
        &self.name
    }
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
pub(super) enum OrdinalState {
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
pub(super) type RaceSettlement = (u64, Result<String, String>);

/// Error from one collect resolution pass.
#[derive(Debug, thiserror::Error)]
pub(super) enum CollectError {
    /// Existing workflow-visible validation or durability failure.
    #[error("{0}")]
    Message(String),
    /// Typed runtime failure that must fail the workflow rather than becoming
    /// a workflow-visible collect result.
    #[error(transparent)]
    Engine(#[from] EngineError),
}

impl From<String> for CollectError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

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
) -> Result<CollectStep, CollectError> {
    if let Some(sentinel) = super::nif_query_pump::take_pending_query_sentinel(state, pid) {
        return Ok(CollectStep::QuerySentinel(sentinel));
    }
    // The SDK never sends an empty list (concurrency.gleam guards), but an
    // empty fan-out resolves immediately and must pin nothing.
    if specs.is_empty() {
        return match kind {
            CollectKind::All => Ok(CollectStep::AllCompleted(Vec::new())),
            CollectKind::Race => Err("expected at least one activity".to_owned().into()),
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
            let scheduled = fresh_scheduled_activity(spec, start_time_task_queue, label)?;
            context
                .record_activity_scheduled_started(
                    Utc::now(),
                    ActivityId::from_sequence_position(*ordinal),
                    scheduled,
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
            super::nif_activity_dispatch::RetryRecorderSeam {
                recorder: context.recorder(),
                run_id: context.workflow_handle().run_id().clone(),
            },
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
                // NOI-0: a fresh fan-out dispatch is the first delivery of each member, attempt 1.
                attempt: FIRST_DELIVERY_ATTEMPT,
            })
        })
        .collect()
}

/// Build the [`ScheduledActivity`](super::nif_activity::ScheduledActivity) for one FRESH
/// (non-recovered) fan-out member on the flag-OFF single-schedule path.
///
/// Resolves the task queue and OPTIONAL node once at this schedule seam (NSTQ-4 / NODE-4, same
/// precedence as the flag-ON fan-out and single-schedule paths), and stamps the first-delivery
/// attempt (NOI-0): a fresh fan-out member is its first delivery, attempt 1.
fn fresh_scheduled_activity(
    spec: &ActivitySpec,
    start_time_task_queue: Option<&str>,
    label: &str,
) -> Result<super::nif_activity::ScheduledActivity, String> {
    Ok(super::nif_activity::ScheduledActivity {
        activity_type: spec.name.clone(),
        input: payload_from_json_text(&spec.input, label)?,
        task_queue: super::nif_activity::resolve_task_queue(&spec.config, start_time_task_queue),
        node: super::nif_activity::resolve_node(&spec.config),
        attempt: FIRST_DELIVERY_ATTEMPT,
    })
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
                // NOI-0 recovery: re-derive the attempt from the recorded `ActivityStarted` so the
                // re-staged dispatch keeps the SAME attempt identity. A pre-field or unrecorded
                // ordinal reads back the legacy sentinel / first delivery deterministically.
                attempt: started_attempt(history, *ordinal).unwrap_or(FIRST_DELIVERY_ATTEMPT),
            })
        })
        .collect()
}

use super::nif_collect_settlement::{
    offset_to_u64, payload_from_json_text, recorded_terminal, scheduled_activity_type,
    scheduled_node, scheduled_task_queue, settle_all, settle_race, started_attempt,
};

#[cfg(test)]
#[path = "nif_collect_tests/mod.rs"]
mod tests;
