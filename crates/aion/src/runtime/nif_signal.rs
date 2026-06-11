//! Signal NIF bridge implementations.

use std::sync::Arc;

use aion_core::{Event, Payload, WorkflowId};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary;
use beamr::term::binary_ref::BinaryRef;
use beamr::term::boxed;
use chrono::Utc;
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome, SignalDelivery};
use crate::engine::delegated::SignalRouter;
use crate::registry::Registry;
use crate::runtime::nif_state::{EngineNifState, PendingAwait};
use crate::runtime::{Pid, RuntimeHandle};
use crate::{EngineError, WorkflowHandle};

thread_local! {
    static NIF_SIGNAL_HEAP: std::cell::RefCell<Vec<Box<[u64]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

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

fn park_heap(heap: Box<[u64]>) {
    NIF_SIGNAL_HEAP.with_borrow_mut(|parked| parked.push(heap));
}

fn alloc_binary_term(bytes: &[u8]) -> Option<Term> {
    let word_count = 2 + binary::packed_word_count(bytes.len());
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = binary::write_binary(&mut heap, bytes)?;
    park_heap(heap);
    Some(term)
}

fn alloc_tuple_term(elements: &[Term]) -> Option<Term> {
    let word_count = 1 + elements.len();
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = boxed::write_tuple(&mut heap, elements)?;
    park_heap(heap);
    Some(term)
}

fn ok_result_term(value: &str) -> Option<Term> {
    let value_term = alloc_binary_term(value.as_bytes())?;
    alloc_tuple_term(&[Term::atom(Atom::OK), value_term])
}

fn error_result_term(message: &str) -> Option<Term> {
    let value_term = alloc_binary_term(message.as_bytes())?;
    alloc_tuple_term(&[Term::atom(Atom::ERROR), value_term])
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
    )
    .map_err(signal_error)?;
    let pinned = match state.pending_awaits.get(&pid).map(|entry| entry.clone()) {
        Some(PendingAwait::Signal { index }) => Some(index),
        Some(PendingAwait::Sleep { .. }) => {
            return Err("receive_signal: process is pinned to a pending sleep await".to_owned());
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
            // pinned slot are both released for the next receive.
            if let Some(message) = super::nif_timeout::expired_scope_message(state, pid) {
                state.pending_awaits.remove(&pid);
                return Err(message);
            }
            state
                .pending_awaits
                .insert(pid, PendingAwait::Signal { index });
            Ok(SignalReceiveOutcome::Suspend)
        }
    }
}

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
    )
    .map_err(signal_error)?;
    let index = signal_occurrence_index(context.history(), name);
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
        ResolveOutcome::Recorded(Resolution::SignalSent) => Ok("delivered".to_owned()),
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
            Ok("delivered".to_owned())
        }
    }
}

pub(super) fn receive_signal(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        return Ok(error_result_term(&format!(
            "receive_signal: expected 2 arguments, got {}",
            args.len()
        ))
        .unwrap_or(Term::NIL));
    }
    let name = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("receive_signal name: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let config = match decode_string_arg(args[1]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("receive_signal config: {error}")).unwrap_or(Term::NIL),
            );
        }
    };
    let Some(pid) = ctx.pid() else {
        return Ok(error_result_term("receive_signal: missing caller pid").unwrap_or(Term::NIL));
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    let bridge = match signal_bridge(ctx) {
        Ok(bridge) => bridge,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    // A query handler must not nest into another await (and must not record
    // a signal receive); refuse before any marker is consumed.
    if let Err(error) =
        super::nif_query_pump::ensure_not_servicing_query(&state, pid, "receive_signal")
    {
        return Ok(error_result_term(&error).unwrap_or(Term::NIL));
    }
    // One wake marker is consumed per invocation; leaving it queued would
    // insta-rewake the suspend below into a busy spin.
    super::nif_wake::consume_wake_marker(ctx, &bridge.runtime);
    // Queries first (Q6): a pending query is serviced before this await's
    // own resolution, so operator queries are never starved by a workflow
    // whose awaits keep resolving immediately.
    if let Some(sentinel) = super::nif_query_pump::take_pending_query_sentinel(&state, pid) {
        return Ok(error_result_term(&sentinel).unwrap_or(Term::NIL));
    }
    match receive_signal_impl(&state, &bridge, &name, &config, pid) {
        Ok(SignalReceiveOutcome::Payload(result)) => {
            Ok(ok_result_term(&result).unwrap_or(Term::NIL))
        }
        Ok(SignalReceiveOutcome::Suspend) => {
            ctx.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(error) => Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    }
}

pub(super) fn send_signal(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 3 {
        return Ok(error_result_term(&format!(
            "send_signal: expected 3 arguments, got {}",
            args.len()
        ))
        .unwrap_or(Term::NIL));
    }
    let target = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("send_signal target: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let name = match decode_string_arg(args[1]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("send_signal name: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let payload = match decode_string_arg(args[2]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("send_signal payload: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let Some(pid) = ctx.pid() else {
        return Ok(error_result_term("send_signal: missing caller pid").unwrap_or(Term::NIL));
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    // send_signal records `SignalSent`; a query handler must stay read-only.
    if let Err(error) =
        super::nif_query_pump::ensure_not_servicing_query(&state, pid, "send_signal")
    {
        return Ok(error_result_term(&error).unwrap_or(Term::NIL));
    }
    let bridge = match signal_bridge(ctx) {
        Ok(bridge) => bridge,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    match send_signal_impl(&bridge, &target, &name, &payload, pid) {
        Ok(result) => Ok(ok_result_term(&result).unwrap_or(Term::NIL)),
        Err(error) => Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    }
}

#[cfg(test)]
mod tests {
    use super::signal_occurrence_index;
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use chrono::Utc;
    use serde_json::json;

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
