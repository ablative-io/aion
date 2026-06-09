//! Signal NIF bridge implementations.

use std::sync::{Arc, OnceLock};

use aion_core::{Event, Payload, WorkflowId};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed;
use chrono::Utc;
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome, SignalDelivery};
use crate::engine::delegated::SignalRouter;
use crate::registry::Registry;
use crate::runtime::{Pid, RuntimeHandle};
use crate::{EngineError, WorkflowHandle};

thread_local! {
    static NIF_SIGNAL_HEAP: std::cell::RefCell<Vec<Box<[u64]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

static SIGNAL_BRIDGE: OnceLock<Arc<SignalNifBridge>> = OnceLock::new();

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

/// Install the process-wide signal NIF bridge.
pub(crate) fn install_signal_nif_bridge(bridge: Arc<SignalNifBridge>) {
    let _ = SIGNAL_BRIDGE.set(bridge);
}

fn signal_bridge() -> Option<Arc<SignalNifBridge>> {
    SIGNAL_BRIDGE.get().cloned()
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
    let bin = Binary::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
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

fn receive_occurrence_index(history: &[Event], name: &str) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::SignalReceived { name: event_name, .. } if event_name == name))
        .count()
}

fn send_occurrence_index(history: &[Event], name: &str) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::SignalSent { name: event_name, .. } if event_name == name))
        .count()
}

fn latest_received_payload(history: &[Event], name: &str, index: usize) -> Option<Payload> {
    history
        .iter()
        .filter_map(|event| match event {
            Event::SignalReceived {
                name: event_name,
                payload,
                ..
            } if event_name == name => Some(payload.clone()),
            _ => None,
        })
        .nth(index)
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

fn receive_signal_impl(name: &str, config: &str, pid: Pid) -> Result<String, String> {
    let _ = config;
    let bridge = signal_bridge().ok_or_else(|| "signal NIF bridge is not configured".to_owned())?;
    let mut context = super::nif_context::NifContext::new(
        pid,
        bridge.registry.as_ref(),
        bridge.tokio_handle.clone(),
    )
    .map_err(signal_error)?;
    let index = receive_occurrence_index(context.history(), name);
    let command = Command::AwaitSignal {
        key: CorrelationKey::Signal {
            name: name.to_owned(),
            index,
        },
    };

    match context.resolve_command(command).map_err(signal_error)? {
        ResolveOutcome::Recorded(Resolution::SignalDelivered(payload)) => {
            payload_to_json_string(&payload)
        }
        ResolveOutcome::Recorded(other) => Err(format!("unexpected signal resolution: {other:?}")),
        ResolveOutcome::ResumeLive => {
            let payload = bridge.runtime.wait_for_signal_message(pid, name);
            let history = context.read_current_history().map_err(signal_error)?;
            if latest_received_payload(&history, name, index).is_none() {
                let record_name = name.to_owned();
                let record_payload = payload.clone();
                context
                    .block_on_recorder(|recorder| {
                        Box::pin(async move {
                            recorder
                                .record_signal_received(Utc::now(), record_name, record_payload)
                                .await
                        })
                    })
                    .map_err(signal_error)?;
            }
            payload_to_json_string(&payload)
        }
    }
}

fn send_signal_impl(
    target: &str,
    name: &str,
    payload_json: &str,
    pid: Pid,
) -> Result<String, String> {
    let bridge = signal_bridge().ok_or_else(|| "signal NIF bridge is not configured".to_owned())?;
    let target_workflow_id = parse_workflow_id(target)?;
    let payload = payload_from_json_string(payload_json)?;
    let mut context = super::nif_context::NifContext::new(
        pid,
        bridge.registry.as_ref(),
        bridge.tokio_handle.clone(),
    )
    .map_err(signal_error)?;
    let index = send_occurrence_index(context.history(), name);
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
    match receive_signal_impl(&name, &config, pid) {
        Ok(result) => Ok(ok_result_term(&result).unwrap_or(Term::NIL)),
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
    match send_signal_impl(&target, &name, &payload, pid) {
        Ok(result) => Ok(ok_result_term(&result).unwrap_or(Term::NIL)),
        Err(error) => Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    }
}

#[cfg(test)]
mod tests {
    use super::{receive_occurrence_index, send_occurrence_index};
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
    fn derives_receive_and_send_occurrences_from_matching_event_kinds()
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
        ];

        assert_eq!(receive_occurrence_index(&history, "ready"), 1);
        assert_eq!(send_occurrence_index(&history, "ready"), 1);
        Ok(())
    }
}
