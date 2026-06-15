//! Signal NIF bridge implementations.

use std::sync::Arc;

use aion_core::{Event, Payload, WorkflowId};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary_ref::BinaryRef;
use chrono::Utc;
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome, SignalDelivery};
use crate::engine::delegated::SignalRouter;
use crate::registry::Registry;
use crate::runtime::nif_state::{EngineNifState, PendingAwait};
use crate::runtime::{Pid, RuntimeHandle};
use crate::{EngineError, WorkflowHandle};

/// Engine-owned signal bridge context used by raw NIF function pointers.
pub(crate) struct SignalNifBridge {
    registry: Arc<Registry>,
    runtime: Arc<RuntimeHandle>,
    tokio_handle: Handle,
    signal_router: Arc<dyn SignalRouter>,
}

impl SignalNifBridge {
    /// Create a signal NIF bridge from engine-owned seams.
    #[must_use]
    pub fn new(
        registry: Arc<Registry>,
        runtime: Arc<RuntimeHandle>,
        tokio_handle: Handle,
        signal_router: Arc<dyn SignalRouter>,
    ) -> Self {
        Self {
            registry,
            runtime,
            tokio_handle,
            signal_router,
        }
    }
}

/// Install the engine-scoped signal NIF bridge.
pub(crate) fn install_signal_nif_bridge(
    state: &super::nif_state::EngineNifState,
    bridge: Arc<SignalNifBridge>,
) {
    match state.signal_bridge.write() {
        Ok(mut slot) => *slot = Some(bridge),
        Err(poisoned) => *poisoned.into_inner() = Some(bridge),
    }
}

fn signal_bridge(ctx: &ProcessContext) -> Result<Arc<SignalNifBridge>, String> {
    let state = super::nif_state::engine_nif_state(ctx)?;
    let slot = match state.signal_bridge.read() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    slot.ok_or_else(|| "signal NIF bridge is not configured".to_owned())
}

/// Build `{ok, <<value>>}` on the calling process heap.
///
/// Result terms are allocated through the [`ProcessContext`] allocators:
/// attached (normal-scheduler) calls get GC-traced process-heap terms, and
/// detached (dirty) calls get owned blocks the dirty-result bridge copies
/// onto the process heap. Nothing is parked in thread-locals — beamr's
/// moving GC never traces out-of-heap pointers, so a parked heap either
/// leaks for the scheduler thread's lifetime or dangles once cleared while
/// workflow code still references the term (N-6).
///
/// Allocation may collect on attached calls: decode every argument `Term`
/// before the first result allocation.
fn ok_result_term(ctx: &mut ProcessContext, value: &str) -> Option<Term> {
    let value_term = ctx.alloc_binary(value.as_bytes()).ok()?;
    ctx.alloc_tuple(&[Term::atom(Atom::OK), value_term]).ok()
}

/// Build `{error, <<message>>}` on the calling process heap (see
/// [`ok_result_term`] for the allocation contract).
fn error_result_term(ctx: &mut ProcessContext, message: &str) -> Option<Term> {
    let value_term = ctx.alloc_binary(message.as_bytes()).ok()?;
    ctx.alloc_tuple(&[Term::atom(Atom::ERROR), value_term]).ok()
}

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = BinaryRef::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn payload_from_json_string(input: &str) -> Result<Payload, String> {
    let value = serde_json::from_str(input).map_err(|error| format!("payload json: {error}"))?;
    Payload::from_json(&value).map_err(|error| format!("payload encode: {error}"))
}

fn payload_to_json_string(payload: &Payload) -> Result<String, String> {
    let value = payload
        .to_json()
        .map_err(|error| format!("payload decode: {error}"))?;
    serde_json::to_string(&value).map_err(|error| format!("payload json: {error}"))
}

fn signal_occurrence_index(history: &[Event], name: &str) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::SignalReceived { name: event_name, .. } | Event::SignalSent { name: event_name, .. } if event_name == name))
        .count()
}

/// Occurrence index (among sends and receives of `name`, history order) of
/// the k-th recorded `SignalReceived` for `name`, if it exists.
///
/// `receive_signal` consumption is positional: the k-th completed receive
/// for a name consumes the k-th recorded arrival. `SignalSent` events share
/// the per-name occurrence keyspace, so an arrival's correlation index can
/// exceed its receive rank when the workflow also sends the same name.
fn nth_received_occurrence_index(history: &[Event], name: &str, k: u64) -> Option<usize> {
    let mut occurrence = 0_usize;
    let mut receives_seen = 0_u64;
    for event in history {
        match event {
            Event::SignalReceived {
                name: event_name, ..
            } if event_name == name => {
                if receives_seen == k {
                    return Some(occurrence);
                }
                receives_seen += 1;
                occurrence += 1;
            }
            Event::SignalSent {
                name: event_name, ..
            } if event_name == name => occurrence += 1,
            _ => {}
        }
    }
    None
}

/// Occurrence index (among sends and receives of `name`, history order) of
/// the k-th recorded `SignalSent` for `name`, if it exists.
///
/// `send_signal` correlation is positional: the k-th completed send for a
/// name replays the k-th recorded `SignalSent` for that name. `SignalReceived`
/// events share the per-name occurrence keyspace, so a send's correlation
/// index can exceed its send rank when the workflow also receives the name.
/// Counting the full run segment instead derived a key PAST the recorded
/// send whenever any same-name occurrence was recorded after it; the replay
/// cursor found no match and the recovered send re-routed the signal and
/// recorded a second `SignalSent` — duplicate delivery.
fn nth_sent_occurrence_index(history: &[Event], name: &str, k: u64) -> Option<usize> {
    let mut occurrence = 0_usize;
    let mut sends_seen = 0_u64;
    for event in history {
        match event {
            Event::SignalSent {
                name: event_name, ..
            } if event_name == name => {
                if sends_seen == k {
                    return Some(occurrence);
                }
                sends_seen += 1;
                occurrence += 1;
            }
            Event::SignalReceived {
                name: event_name, ..
            } if event_name == name => occurrence += 1,
            _ => {}
        }
    }
    None
}

/// Envelope sequence of the recorded event at per-name occurrence `index`.
///
/// Occurrence indices count `SignalReceived` and `SignalSent` events of
/// `name` in history order — the same keyspace the correlation key uses —
/// so the pinned await's `index` locates exactly the arrival its resolution
/// matched.
fn signal_occurrence_seq(history: &[Event], name: &str, index: usize) -> Option<u64> {
    let mut occurrence = 0_usize;
    for event in history {
        match event {
            Event::SignalReceived {
                envelope,
                name: event_name,
                ..
            }
            | Event::SignalSent {
                envelope,
                name: event_name,
                ..
            } if event_name == name => {
                if occurrence == index {
                    return Some(envelope.seq);
                }
                occurrence += 1;
            }
            _ => {}
        }
    }
    None
}

/// F1b for signals — order an enclosing expired `with_timeout` deadline
/// against the recorded arrival, identically on the live and replayed paths.
///
/// The signal router records arrivals asynchronously, so a deadline
/// `TimerFired` and a `SignalReceived` can both be in the run segment when
/// this await resolves. History order is the truth both paths share: an
/// arrival recorded after the deadline fired was never observed by the live
/// run (it took the timeout branch), so resolution takes the timeout branch
/// — and consumes nothing, leaving the arrival for a later receive — on
/// live and replay alike. Without this rule a live run that timed out
/// replayed into the payload branch and sheared the per-name occurrence
/// index for every later receive (N-2).
fn scope_expired_before_signal_arrival(
    state: &EngineNifState,
    context: &super::nif_context::NifContext,
    pid: Pid,
    name: &str,
    index: usize,
) -> bool {
    let Some(deadline) = super::nif_timeout::expired_scope_deadline(state, pid, context.history())
    else {
        return false;
    };
    match (
        deadline,
        signal_occurrence_seq(context.history(), name, index),
    ) {
        (super::nif_timeout::ExpiredScopeDeadline::RecordedAt(fired_seq), Some(arrival_seq)) => {
            fired_seq < arrival_seq
        }
        // An expiry without a recorded position orders before every arrival
        // (replay-derived scope state), and a resolved arrival must be in
        // the segment — if it cannot be located the deterministic choice is
        // the deadline branch either way.
        (super::nif_timeout::ExpiredScopeDeadline::Unordered, _)
        | (super::nif_timeout::ExpiredScopeDeadline::RecordedAt(_), None) => true,
    }
}

fn parse_workflow_id(value: &str) -> Result<WorkflowId, String> {
    let uuid = Uuid::parse_str(value).map_err(|error| format!("workflow_id: {error}"))?;
    Ok(WorkflowId::new(uuid))
}

fn resolve_target(registry: &Registry, target: &WorkflowId) -> Result<WorkflowHandle, String> {
    registry
        .list()
        .map_err(|error| format!("registry: {error}"))?
        .into_iter()
        .find(|handle| handle.workflow_id() == target)
        .ok_or_else(|| format!("workflow_not_found:{target}"))
}

fn signal_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Outcome of one `receive_signal` invocation.
#[derive(Debug)]
enum SignalReceiveOutcome {
    /// The awaited signal's recorded payload, JSON-encoded.
    Payload(String),
    /// Park the calling process; a mailbox wake re-invokes the native.
    Suspend,
}

/// Two-phase signal await: the native never blocks a scheduler thread.
///
/// The awaited occurrence index is pinned at first arrival (the suspended
/// re-entries and crash-recovery replay must resolve the same logical
/// occurrence). The signal router records `SignalReceived` durably before
/// waking the process, so resolution always reads recorded history — this
/// native never records arrivals itself.
fn receive_signal_impl(
    state: &EngineNifState,
    bridge: &Arc<SignalNifBridge>,
    name: &str,
    config: &str,
    pid: Pid,
) -> Result<SignalReceiveOutcome, String> {
    let _ = config;
    let mut context = super::nif_context::NifContext::new(
        pid,
        bridge.registry.as_ref(),
        bridge.tokio_handle.clone(),
        bridge.runtime.signal_delivery(),
    )
    .map_err(signal_error)?;
    let pinned = match state.pending_awaits.get(&pid).map(|entry| entry.clone()) {
        Some(PendingAwait::Signal { index }) => Some(index),
        Some(PendingAwait::Sleep { .. }) => {
            return Err("receive_signal: process is pinned to a pending sleep await".to_owned());
        }
        Some(PendingAwait::Child { .. }) => {
            return Err("receive_signal: process is pinned to a pending child await".to_owned());
        }
        Some(PendingAwait::Collect { .. }) => {
            return Err("receive_signal: process is pinned to a pending collect await".to_owned());
        }
        None => None,
    };
    let index = if let Some(index) = pinned {
        index
    } else {
        // The k-th completed receive consumes the k-th recorded arrival
        // (early arrivals are already in history); with none recorded yet,
        // await the next occurrence slot for this name.
        let consumed = context.signal_receives_consumed(name);
        nth_received_occurrence_index(context.history(), name, consumed)
            .unwrap_or_else(|| signal_occurrence_index(context.history(), name))
    };
    let command = Command::AwaitSignal {
        key: CorrelationKey::Signal {
            name: name.to_owned(),
            index,
        },
    };

    match context.resolve_command(command).map_err(signal_error)? {
        ResolveOutcome::Recorded(Resolution::SignalDelivered(payload)) => {
            // F1b: an arrival recorded after the expired deadline's
            // `TimerFired` was never observed live — take the timeout
            // branch, releasing the pin without consuming the occurrence.
            if scope_expired_before_signal_arrival(state, &context, pid, name, index) {
                state.pending_awaits.remove(&pid);
                return Err(super::nif_timeout::SCOPE_EXPIRED_MESSAGE.to_owned());
            }
            state.pending_awaits.remove(&pid);
            context.mark_signal_receive_consumed(name);
            Ok(SignalReceiveOutcome::Payload(payload_to_json_string(
                &payload,
            )?))
        }
        ResolveOutcome::Recorded(other) => Err(format!("unexpected signal resolution: {other:?}")),
        ResolveOutcome::ResumeLive => {
            // An expired enclosing with_timeout deadline aborts the await;
            // a timed-out receive consumes nothing, so the count and the
            // pinned slot are both released for the next receive. The expiry
            // decision is a pure function of the RESOLUTION snapshot
            // (`context.history()`), never a fresh store read: this
            // resolution observed neither an arrival nor the deadline's
            // `TimerFired`, so it suspends and converges to the Recorded
            // path (with its F1b ordering) on the next wake — a fresh read
            // here let live time out where replay resolved a later-recorded
            // arrival (N-2, the N-1 twin).
            if super::nif_timeout::expired_scope_deadline(state, pid, context.history()).is_some() {
                state.pending_awaits.remove(&pid);
                return Err(super::nif_timeout::SCOPE_EXPIRED_MESSAGE.to_owned());
            }
            state
                .pending_awaits
                .insert(pid, PendingAwait::Signal { index });
            Ok(SignalReceiveOutcome::Suspend)
        }
    }
}

/// Routes the signal to the target first, then records `SignalSent`: a crash
/// between router delivery and the durable record re-routes the send on
/// recovery, so delivery is at-least-once across that window. Closing it
/// would require an outbox / two-phase record.
fn send_signal_impl(
    bridge: &Arc<SignalNifBridge>,
    target: &str,
    name: &str,
    payload_json: &str,
    pid: Pid,
) -> Result<String, String> {
    let target_workflow_id = parse_workflow_id(target)?;
    let payload = payload_from_json_string(payload_json)?;
    let mut context = super::nif_context::NifContext::new(
        pid,
        bridge.registry.as_ref(),
        bridge.tokio_handle.clone(),
        bridge.runtime.signal_delivery(),
    )
    .map_err(signal_error)?;
    // The k-th completed send for a name replays the k-th recorded
    // `SignalSent` for that name (same-name arrivals share the occurrence
    // keyspace and may be recorded after the send); with no k-th send
    // recorded, the next free occurrence slot — matching no recorded event —
    // keys the live path.
    let completed = context.signal_sends_completed(name);
    let index = nth_sent_occurrence_index(context.history(), name, completed)
        .unwrap_or_else(|| signal_occurrence_index(context.history(), name));
    let delivery = SignalDelivery {
        target_workflow_id: target_workflow_id.clone(),
        name: name.to_owned(),
        payload: payload.clone(),
    };
    let command = Command::SendSignal {
        key: CorrelationKey::Signal {
            name: name.to_owned(),
            index,
        },
        delivery: delivery.clone(),
    };

    match context.resolve_command(command).map_err(signal_error)? {
        ResolveOutcome::Recorded(Resolution::SignalSent) => {
            context.mark_signal_send_completed(name);
            Ok("delivered".to_owned())
        }
        ResolveOutcome::Recorded(other) => Err(format!("unexpected signal resolution: {other:?}")),
        ResolveOutcome::ResumeLive => {
            let target_handle = resolve_target(bridge.registry.as_ref(), &target_workflow_id)?;
            bridge
                .tokio_handle
                .block_on(bridge.signal_router.route(
                    &target_handle,
                    delivery.name.clone(),
                    delivery.payload.clone(),
                ))
                .map_err(|error: EngineError| error.to_string())?;
            context
                .block_on_recorder(|recorder| {
                    Box::pin(async move {
                        recorder
                            .record_signal_sent(
                                Utc::now(),
                                delivery.target_workflow_id,
                                delivery.name,
                                delivery.payload,
                            )
                            .await
                    })
                })
                .map_err(signal_error)?;
            context.mark_signal_send_completed(name);
            Ok("delivered".to_owned())
        }
    }
}

pub(super) fn receive_signal(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        return Ok(error_result_term(
            ctx,
            &format!("receive_signal: expected 2 arguments, got {}", args.len()),
        )
        .unwrap_or(Term::NIL));
    }
    let name = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(ctx, &format!("receive_signal name: {error}"))
                    .unwrap_or(Term::NIL),
            );
        }
    };
    let config = match decode_string_arg(args[1]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(ctx, &format!("receive_signal config: {error}"))
                    .unwrap_or(Term::NIL),
            );
        }
    };
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(ctx, "receive_signal: missing caller pid").unwrap_or(Term::NIL)
        );
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    let bridge = match signal_bridge(ctx) {
        Ok(bridge) => bridge,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    // A query handler must not nest into another await (and must not record
    // a signal receive); refuse before any marker is consumed.
    if let Err(error) =
        super::nif_query_pump::ensure_not_servicing_query(&state, pid, "receive_signal")
    {
        return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL));
    }
    // One wake marker is consumed per invocation; leaving it queued would
    // insta-rewake the suspend below into a busy spin.
    super::nif_wake::consume_wake_marker(ctx, &bridge.runtime);
    // Queries first (Q6): a pending query is serviced before this await's
    // own resolution, so operator queries are never starved by a workflow
    // whose awaits keep resolving immediately.
    if let Some(sentinel) = super::nif_query_pump::take_pending_query_sentinel(&state, pid) {
        return Ok(error_result_term(ctx, &sentinel).unwrap_or(Term::NIL));
    }
    match receive_signal_impl(&state, &bridge, &name, &config, pid) {
        Ok(SignalReceiveOutcome::Payload(result)) => {
            Ok(ok_result_term(ctx, &result).unwrap_or(Term::NIL))
        }
        Ok(SignalReceiveOutcome::Suspend) => {
            ctx.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(error) => Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    }
}

pub(super) fn send_signal(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 3 {
        return Ok(error_result_term(
            ctx,
            &format!("send_signal: expected 3 arguments, got {}", args.len()),
        )
        .unwrap_or(Term::NIL));
    }
    let target = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(ctx, &format!("send_signal target: {error}"))
                    .unwrap_or(Term::NIL),
            );
        }
    };
    let name = match decode_string_arg(args[1]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(ctx, &format!("send_signal name: {error}")).unwrap_or(Term::NIL),
            );
        }
    };
    let payload = match decode_string_arg(args[2]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(ctx, &format!("send_signal payload: {error}"))
                    .unwrap_or(Term::NIL),
            );
        }
    };
    let Some(pid) = ctx.pid() else {
        return Ok(error_result_term(ctx, "send_signal: missing caller pid").unwrap_or(Term::NIL));
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    // send_signal records `SignalSent`; a query handler must stay read-only.
    if let Err(error) =
        super::nif_query_pump::ensure_not_servicing_query(&state, pid, "send_signal")
    {
        return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL));
    }
    let bridge = match signal_bridge(ctx) {
        Ok(bridge) => bridge,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    match send_signal_impl(&bridge, &target, &name, &payload, pid) {
        Ok(result) => Ok(ok_result_term(ctx, &result).unwrap_or(Term::NIL)),
        Err(error) => Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        SignalNifBridge, SignalReceiveOutcome, receive_signal_impl, send_signal_impl,
        signal_occurrence_index,
    };
    use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore, WriteToken};
    use async_trait::async_trait;
    use chrono::Utc;
    use serde_json::json;

    use crate::EngineError;
    use crate::engine::delegated::SignalRouter;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_state::{EngineNifState, PendingAwait};
    use crate::runtime::nif_timeout::TimeoutScope;
    use crate::runtime::{RuntimeConfig, RuntimeHandle};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The Recorded-path tests never route; live sends are out of scope.
    struct RejectingRouter;

    #[async_trait]
    impl SignalRouter for RejectingRouter {
        async fn route(
            &self,
            _target: &WorkflowHandle,
            _name: String,
            _payload: Payload,
        ) -> Result<(), EngineError> {
            Err(EngineError::Runtime {
                reason: "test router must not be reached".to_owned(),
            })
        }
    }

    /// Counts live deliveries so duplicate-delivery tests can assert that a
    /// replayed send never reaches the router again.
    #[derive(Default)]
    struct CountingRouter {
        routes: std::sync::atomic::AtomicUsize,
    }

    impl CountingRouter {
        fn routes(&self) -> usize {
            self.routes.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl SignalRouter for CountingRouter {
        async fn route(
            &self,
            _target: &WorkflowHandle,
            _name: String,
            _payload: Payload,
        ) -> Result<(), EngineError> {
            self.routes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    /// Everything one `receive_signal_impl` determinism test needs over a
    /// synthesized parent history.
    struct SignalHarness {
        state: Arc<EngineNifState>,
        bridge: Arc<SignalNifBridge>,
        handle: WorkflowHandle,
        runtime: Arc<RuntimeHandle>,
        registry: Arc<Registry>,
        store: Arc<dyn EventStore>,
        pid: u64,
    }

    impl SignalHarness {
        async fn over_history(pid: u64, extra_events: &[Event]) -> TestHarness {
            Self::over_history_with(
                pid,
                extra_events,
                Arc::new(InMemoryStore::default()),
                Arc::new(RejectingRouter),
            )
            .await
        }

        /// Like [`Self::over_history`], with the event store and signal
        /// router chosen by the test (stale-read races, delivery counting).
        async fn over_history_with(
            pid: u64,
            extra_events: &[Event],
            store: Arc<dyn EventStore>,
            router: Arc<dyn SignalRouter>,
        ) -> TestHarness {
            let registry = Arc::new(Registry::default());
            let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
            let workflow_id = WorkflowId::new_v4();
            let run_id = RunId::new_v4();
            let mut events = vec![Event::WorkflowStarted {
                envelope: EventEnvelope {
                    seq: 1,
                    recorded_at: Utc::now(),
                    workflow_id: workflow_id.clone(),
                },
                workflow_type: "receiver".to_owned(),
                input: Payload::from_json(&json!({}))?,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            }];
            events.extend_from_slice(extra_events);
            let head = events.len() as u64;
            store
                .append(WriteToken::recorder(), &workflow_id, &events, 0)
                .await?;
            let recorder = crate::durability::Recorder::resume_at(
                workflow_id.clone(),
                Arc::clone(&store),
                head,
            );
            let handle = WorkflowHandle::new(WorkflowHandleParts {
                workflow_id: workflow_id.clone(),
                run_id: run_id.clone(),
                pid,
                workflow_type: "receiver".to_owned(),
                namespace: String::from("default"),
                loaded_version: ContentHash::from_bytes([5; 32]),
                cached_status: WorkflowStatus::Running,
                residency: HandleResidency::Resident,
                recorder,
                completion: CompletionNotifier::new(),
            });
            registry.insert((workflow_id, run_id), handle.clone())?;
            let bridge = Arc::new(SignalNifBridge::new(
                Arc::clone(&registry),
                Arc::clone(&runtime),
                tokio::runtime::Handle::current(),
                router,
            ));
            Ok(Self {
                state: Arc::new(EngineNifState::default()),
                bridge,
                handle,
                runtime,
                registry,
                store,
                pid,
            })
        }

        fn receive(&self, name: &str) -> Result<SignalReceiveOutcome, String> {
            tokio::task::block_in_place(|| {
                receive_signal_impl(&self.state, &self.bridge, name, "{}", self.pid)
            })
        }

        fn send(&self, target: &str, name: &str, payload_json: &str) -> Result<String, String> {
            tokio::task::block_in_place(|| {
                send_signal_impl(&self.bridge, target, name, payload_json, self.pid)
            })
        }

        async fn history_len(&self) -> Result<usize, Box<dyn std::error::Error>> {
            let recorder = self.handle.recorder();
            let recorder = recorder.lock().await;
            Ok(recorder.read_history().await?.len())
        }

        fn expire_replayed_scope(&self, deadline: aion_core::TimerId) {
            self.state.timeout_scopes.insert(
                1,
                TimeoutScope::replayed_expired_with_deadline_for_test(self.pid, deadline),
            );
            self.state.timeout_scope_stacks.insert(self.pid, vec![1]);
        }

        fn clear_scopes(&self) {
            self.state.timeout_scopes.clear();
            self.state.timeout_scope_stacks.clear();
        }

        fn shutdown(self) -> TestResult {
            self.runtime.shutdown()?;
            Ok(())
        }
    }

    type TestHarness = Result<SignalHarness, Box<dyn std::error::Error>>;

    fn envelope_for(workflow_id: &WorkflowId, seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: workflow_id.clone(),
        }
    }

    fn received(
        workflow_id: &WorkflowId,
        seq: u64,
        name: &str,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::SignalReceived {
            envelope: envelope_for(workflow_id, seq),
            name: name.to_owned(),
            payload: Payload::from_json(&json!({"n": seq}))?,
        })
    }

    fn fired(workflow_id: &WorkflowId, seq: u64, ordinal: u64) -> Event {
        Event::TimerFired {
            envelope: envelope_for(workflow_id, seq),
            timer_id: aion_core::TimerId::anonymous(ordinal),
        }
    }

    fn sent(
        workflow_id: &WorkflowId,
        seq: u64,
        name: &str,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::SignalSent {
            envelope: envelope_for(workflow_id, seq),
            target_workflow_id: WorkflowId::new_v4(),
            name: name.to_owned(),
            payload: Payload::from_json(&json!({"n": seq}))?,
        })
    }

    /// Crash-recovery duplicate delivery: a `SignalSent` whose name occurs
    /// again later in history (here an arrival of the same name recorded
    /// after the send) must still replay as `Recorded(SignalSent)`. Counting
    /// the FULL run segment for the send's occurrence key derives an index
    /// PAST the recorded send, the cursor finds no match, and the send takes
    /// the live path on recovery — re-routing the signal and recording a
    /// second `SignalSent`.
    #[tokio::test(flavor = "multi_thread")]
    async fn recovered_send_replays_recorded_sent_despite_later_same_name_arrival() -> TestResult {
        let pid = 421;
        let envelope_id = WorkflowId::new_v4();
        let router = Arc::new(CountingRouter::default());
        // Recorded run: send_signal("go") landed at seq 2, then an arrival
        // of the SAME name landed at seq 3. Crash; this fresh harness (zeroed
        // per-run counters over the full recorded history) is the recovery
        // replay.
        let harness = SignalHarness::over_history_with(
            pid,
            &[
                sent(&envelope_id, 2, "go")?,
                received(&envelope_id, 3, "go")?,
            ],
            Arc::new(InMemoryStore::default()),
            Arc::clone(&router) as Arc<dyn SignalRouter>,
        )
        .await?;
        // Self-target: registered, so an erroneous live path routes for real.
        let target = harness.handle.workflow_id().to_string();

        let outcome = harness.send(&target, "go", "{\"ok\":true}");
        assert_eq!(outcome.as_deref(), Ok("delivered"));
        assert_eq!(
            router.routes(),
            0,
            "the recovered send must not deliver the signal a second time"
        );
        assert_eq!(
            harness.history_len().await?,
            3,
            "the recovered send must not record a second SignalSent"
        );

        // The replayed receive still consumes the recorded arrival at its
        // recorded occurrence slot.
        match harness.receive("go") {
            Ok(SignalReceiveOutcome::Payload(payload)) => {
                assert!(payload.contains('3'), "payload: {payload}");
            }
            other => {
                return Err(
                    format!("the recorded arrival must resolve on replay: {other:?}").into(),
                );
            }
        }
        assert_eq!(
            harness.history_len().await?,
            3,
            "replay of the recovered history must append zero events"
        );
        harness.shutdown()
    }

    /// Replay stability across the interleaving classes: multiple same-name
    /// sends, an arrival of the same name between them, and foreign events
    /// (a scope deadline's `TimerFired`) inside the segment. Replaying the
    /// recovered run's calls in order must resolve every one from history —
    /// zero routes, zero appended events.
    #[tokio::test(flavor = "multi_thread")]
    async fn recovered_interleaved_sends_and_receives_replay_without_appends() -> TestResult {
        let pid = 422;
        let envelope_id = WorkflowId::new_v4();
        let router = Arc::new(CountingRouter::default());
        // Live order was: send (2), receive resolved the arrival (3), the
        // enclosing scope's deadline fired (4), send (5). Crash; replay.
        let harness = SignalHarness::over_history_with(
            pid,
            &[
                sent(&envelope_id, 2, "go")?,
                received(&envelope_id, 3, "go")?,
                fired(&envelope_id, 4, 7),
                sent(&envelope_id, 5, "go")?,
            ],
            Arc::new(InMemoryStore::default()),
            Arc::clone(&router) as Arc<dyn SignalRouter>,
        )
        .await?;
        let target = harness.handle.workflow_id().to_string();

        assert_eq!(
            harness.send(&target, "go", "{\"k\":0}").as_deref(),
            Ok("delivered")
        );
        match harness.receive("go") {
            Ok(SignalReceiveOutcome::Payload(payload)) => {
                assert!(payload.contains('3'), "payload: {payload}");
            }
            other => {
                return Err(
                    format!("the recorded arrival must resolve on replay: {other:?}").into(),
                );
            }
        }
        assert_eq!(
            harness.send(&target, "go", "{\"k\":1}").as_deref(),
            Ok("delivered")
        );

        assert_eq!(router.routes(), 0, "replay must deliver nothing twice");
        assert_eq!(
            harness.history_len().await?,
            5,
            "replay of the recovered history must append zero events"
        );
        harness.shutdown()
    }

    /// N-2 (Recorded path, F1b): a signal recorded AFTER the expired
    /// deadline's `TimerFired` was never observed by the live run — the
    /// replayed receive must take the timeout branch and consume nothing.
    /// Before the fix this resolved the payload and advanced the per-name
    /// receive count, shearing the occurrence index for every later
    /// `receive_signal` of the name after recovery.
    #[tokio::test(flavor = "multi_thread")]
    async fn signal_recorded_after_deadline_takes_the_timeout_branch() -> TestResult {
        let pid = 411;
        // The racing history: deadline fired (seq 2), then the arrival
        // landed (seq 3). Envelope workflow ids are payload-only here.
        let envelope_id = WorkflowId::new_v4();
        let harness = SignalHarness::over_history(
            pid,
            &[fired(&envelope_id, 2, 9), received(&envelope_id, 3, "go")?],
        )
        .await?;
        harness.expire_replayed_scope(aion_core::TimerId::anonymous(9));

        let outcome = harness.receive("go");
        assert_eq!(
            outcome.err().as_deref(),
            Some("timeout:deadline expired"),
            "an arrival recorded after the deadline must not resolve (N-2)"
        );
        assert!(harness.state.pending_awaits.get(&pid).is_none());
        assert_eq!(
            harness.handle.signal_receives_consumed("go"),
            0,
            "the timed-out receive must consume nothing"
        );

        // Occurrence-index consistency: the next receive (after the scope
        // unwinds) resolves exactly the recorded arrival.
        harness.clear_scopes();
        match harness.receive("go") {
            Ok(SignalReceiveOutcome::Payload(payload)) => {
                assert!(payload.contains('3'), "payload: {payload}");
            }
            other => {
                return Err(format!(
                    "the arrival must remain consumable after the timeout: {other:?}"
                )
                .into());
            }
        }
        assert_eq!(harness.handle.signal_receives_consumed("go"), 1);
        harness.shutdown()
    }

    /// F1b converse: an arrival recorded BEFORE the deadline fired was
    /// observed by the live run, so live and replay both resolve the payload
    /// even though the scope is expired by resolution time.
    #[tokio::test(flavor = "multi_thread")]
    async fn signal_recorded_before_deadline_resolves_the_payload() -> TestResult {
        let pid = 412;
        let envelope_id = WorkflowId::new_v4();
        let harness = SignalHarness::over_history(
            pid,
            &[received(&envelope_id, 2, "go")?, fired(&envelope_id, 3, 9)],
        )
        .await?;
        harness.expire_replayed_scope(aion_core::TimerId::anonymous(9));

        match harness.receive("go") {
            Ok(SignalReceiveOutcome::Payload(payload)) => {
                assert!(payload.contains('2'), "payload: {payload}");
            }
            other => {
                return Err(format!(
                    "an arrival recorded before the deadline must resolve: {other:?}"
                )
                .into());
            }
        }
        assert_eq!(harness.handle.signal_receives_consumed("go"), 1);
        assert!(harness.state.pending_awaits.get(&pid).is_none());
        harness.shutdown()
    }

    /// N-2 (`ResumeLive` path): with neither the arrival nor the deadline's
    /// `TimerFired` in the resolution snapshot, the receive must suspend —
    /// never decide the timeout branch from a fresh store read. Race
    /// modeled with a stale-read store: the durable history already holds
    /// [arrival(2), TimerFired(3)] but step 1's resolution read is truncated
    /// to the `WorkflowStarted` prefix. Before the fix `expired_scope_message`
    /// re-read the store via the timer bridge, saw the fired deadline, and
    /// timed the live receive out — while replay resolved the arrival via
    /// F1b (2 < 3): opposite branches.
    #[tokio::test(flavor = "multi_thread")]
    async fn live_expiry_is_decided_from_the_resolution_snapshot_only() -> TestResult {
        let pid = 413;
        let envelope_id = WorkflowId::new_v4();
        let scope_timer = aion_core::TimerId::anonymous(9);
        // Durable truth: arrival (seq 2) BEFORE the deadline (seq 3); step
        // 1's resolution read sees only the WorkflowStarted prefix.
        let backing = Arc::new(crate::runtime::nif_test_stores::StaleReadStore::new(1));
        let harness = SignalHarness::over_history_with(
            pid,
            &[received(&envelope_id, 2, "go")?, fired(&envelope_id, 3, 9)],
            Arc::clone(&backing) as Arc<dyn EventStore>,
            Arc::new(RejectingRouter),
        )
        .await?;
        backing.set_stale_target(harness.handle.workflow_id(), 1);
        // The timer bridge backs the OLD fresh-read path; installing it
        // proves this test fails pre-fix instead of accidentally passing
        // because the fresh read was unavailable.
        crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
            &harness.state,
            Arc::clone(&harness.registry),
            Arc::clone(&harness.store),
            tokio::runtime::Handle::current(),
            crate::runtime::SignalDeliveryConfig::default(),
        );
        // Live scope whose deadline is the recorded TimerFired(seq 3).
        harness
            .state
            .timeout_scopes
            .insert(2, TimeoutScope::live_for_test(pid, scope_timer));
        harness.state.timeout_scope_stacks.insert(pid, vec![2]);

        // Step 1 — stale resolution snapshot (neither event): must park,
        // never decide the timeout branch from a fresh store read.
        match harness.receive("go") {
            Ok(SignalReceiveOutcome::Suspend) => {}
            other => {
                return Err(format!(
                    "a snapshot lacking both events must park, not branch: {other:?}"
                )
                .into());
            }
        }
        assert_eq!(
            harness.state.pending_awaits.get(&pid).map(|e| e.clone()),
            Some(PendingAwait::Signal { index: 0 })
        );

        // The wake re-entry reads the full history and converges on the
        // Recorded path: F1b orders the arrival before the deadline and
        // resolves the payload — exactly what replay derives from the same
        // history.
        match harness.receive("go") {
            Ok(SignalReceiveOutcome::Payload(payload)) => {
                assert!(payload.contains('2'), "payload: {payload}");
            }
            other => {
                return Err(format!(
                    "the converged re-entry must resolve the earlier arrival: {other:?}"
                )
                .into());
            }
        }
        assert_eq!(harness.handle.signal_receives_consumed("go"), 1);
        harness.shutdown()
    }

    fn payload() -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(&json!({ "ok": true }))?)
    }

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: WorkflowId::new_v4(),
        }
    }

    #[test]
    fn derives_signal_occurrences_from_matching_signal_names()
    -> Result<(), Box<dyn std::error::Error>> {
        let history = vec![
            Event::SignalReceived {
                envelope: envelope(1),
                name: "ready".to_owned(),
                payload: payload()?,
            },
            Event::SignalSent {
                envelope: envelope(2),
                target_workflow_id: WorkflowId::new_v4(),
                name: "ready".to_owned(),
                payload: payload()?,
            },
            Event::SignalSent {
                envelope: envelope(3),
                target_workflow_id: WorkflowId::new_v4(),
                name: "other".to_owned(),
                payload: payload()?,
            },
        ];

        assert_eq!(signal_occurrence_index(&history, "ready"), 2);
        assert_eq!(signal_occurrence_index(&history, "other"), 1);
        Ok(())
    }
}
