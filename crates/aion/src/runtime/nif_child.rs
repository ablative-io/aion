//! Child-workflow NIF bridge implementations.

use std::sync::{Arc, OnceLock};

use aion_core::{ContentType, Payload, RunId, WorkflowError, WorkflowId};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed;

use crate::child::{ChildWorkflowError, ChildWorkflowRecordingContext, await_child, spawn};
use crate::durability::{Command, CorrelationKey, DurabilityError, Resolution, ResolveOutcome};
use crate::runtime::nif_child_engine::{ChildNifBridge, CompletionMailbox, NifChildEngine};

use super::nif_context::{NifContext, NifContextError};

thread_local! {
    static CHILD_NIF_HEAP: std::cell::RefCell<Vec<Box<[u64]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

static CHILD_BRIDGE: OnceLock<Arc<ChildNifBridge>> = OnceLock::new();

/// Installs the engine-owned dependencies used by child workflow NIFs.
pub(crate) fn install_child_nif_bridge(bridge: Arc<ChildNifBridge>) {
    let _ = CHILD_BRIDGE.set(bridge);
}

/// NIF backing `aion_flow_ffi:spawn_child/3`.
pub(super) fn spawn_child_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    Ok(checked_child_result(
        run_spawn_child(args, ctx),
        "spawn_child",
    ))
}

/// NIF backing `aion_flow_ffi:await_child/1`.
pub(super) fn await_child_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    Ok(checked_child_result(
        run_await_child(args, ctx),
        "await_child",
    ))
}

fn run_spawn_child(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, String> {
    require_arity("spawn_child", args, 3)?;
    let workflow_type =
        decode_string_arg(args[0]).map_err(|error| format!("workflow_type:{error}"))?;
    let input = decode_payload_arg(args[1]).map_err(|error| format!("input:{error}"))?;
    decode_string_arg(args[2]).map_err(|error| format!("options:{error}"))?;
    let bridge = child_bridge()?;
    let pid = ctx.pid().ok_or_else(|| "missing_caller_pid".to_owned())?;
    let mut nif = new_context(&bridge, pid)?;
    let key = next_child_key(&nif)?;
    let command = Command::SpawnChild {
        key,
        workflow_type: workflow_type.clone(),
        input: input.clone(),
    };

    match nif
        .resolve_command(command)
        .map_err(|error| context_error(&error))?
    {
        ResolveOutcome::Recorded(Resolution::ChildStarted(child_id)) => {
            term_or_encoding_error(ok_result_term(&child_id.to_string()))
        }
        ResolveOutcome::Recorded(other) => {
            Err(format!("unexpected_child_spawn_resolution:{other:?}"))
        }
        ResolveOutcome::ResumeLive => {
            let engine = NifChildEngine::new(Arc::clone(&bridge), nif.workflow_handle().clone());
            let mut recording = recording_context(&nif)?;
            let child = spawn(
                &engine,
                &mut recording,
                workflow_type,
                input,
                RunId::new_v4(),
            )
            .map_err(|error| child_error(&error))?;
            term_or_encoding_error(ok_result_term(&child.child_workflow_id.to_string()))
        }
    }
}

fn run_await_child(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, String> {
    require_arity("await_child", args, 1)?;
    let child_workflow_id = parse_workflow_id(&decode_string_arg(args[0])?)?;
    let bridge = child_bridge()?;
    let pid = ctx.pid().ok_or_else(|| "missing_caller_pid".to_owned())?;
    let mut nif = new_context(&bridge, pid)?;
    let command = Command::AwaitChild {
        child_workflow_id: child_workflow_id.clone(),
    };

    match nif
        .resolve_command(command)
        .map_err(|error| context_error(&error))?
    {
        ResolveOutcome::Recorded(Resolution::ChildCompleted(result)) => {
            term_or_encoding_error(ok_result_term(&payload_text(&result)?))
        }
        ResolveOutcome::Recorded(Resolution::ChildFailed(error)) => {
            term_or_encoding_error(error_result_term(&workflow_error_text(&error)))
        }
        ResolveOutcome::Recorded(other) => {
            Err(format!("unexpected_child_await_resolution:{other:?}"))
        }
        ResolveOutcome::ResumeLive => {
            let engine = NifChildEngine::new(Arc::clone(&bridge), nif.workflow_handle().clone());
            let mut recording = recording_context(&nif)?;
            let mut mailbox = CompletionMailbox::new(&bridge, &child_workflow_id)?;
            match await_child(&engine, &mut recording, &mut mailbox, &child_workflow_id) {
                Ok(result) => term_or_encoding_error(ok_result_term(&payload_text(&result)?)),
                Err(ChildWorkflowError::Failed { error, .. }) => {
                    term_or_encoding_error(error_result_term(&workflow_error_text(&error)))
                }
                Err(error) => Err(child_error(&error)),
            }
        }
    }
}

fn checked_child_result(result: Result<Term, String>, name: &str) -> Term {
    match result {
        Ok(term) => term,
        Err(message) => error_result_term(&format!("{name}:{message}")).unwrap_or(Term::NIL),
    }
}

fn child_bridge() -> Result<Arc<ChildNifBridge>, String> {
    CHILD_BRIDGE
        .get()
        .cloned()
        .ok_or_else(|| "no_child_nif_bridge_configured".to_owned())
}

fn new_context(bridge: &ChildNifBridge, pid: u64) -> Result<NifContext, String> {
    NifContext::new_with_history_store(
        pid,
        bridge.registry(),
        bridge.tokio_handle(),
        Some(bridge.store()),
    )
    .map_err(|error| context_error(&error))
}

fn next_child_key(nif: &NifContext) -> Result<CorrelationKey, String> {
    let next_seq = next_sequence(nif.current_recorder_head())
        .map_err(|error| context_error(&NifContextError::Durability(error)))?;
    Ok(CorrelationKey::Child(next_seq))
}

fn recording_context(nif: &NifContext) -> Result<ChildWorkflowRecordingContext, String> {
    let next_seq = next_sequence(nif.current_recorder_head())
        .map_err(|error| context_error(&NifContextError::Durability(error)))?;
    Ok(ChildWorkflowRecordingContext::new(
        nif.workflow_id().clone(),
        next_seq,
        nif.last_recorded_at()
            .ok_or_else(|| "child NIF history has no workflow start timestamp".to_owned())?,
    ))
}

fn next_sequence(head: u64) -> Result<u64, DurabilityError> {
    head.checked_add(1)
        .ok_or_else(|| DurabilityError::HistoryShape {
            reason: format!("child NIF sequence overflow advancing {head} by 1"),
        })
}

fn context_error(error: &NifContextError) -> String {
    error.to_string()
}

fn child_error(error: &ChildWorkflowError) -> String {
    error.to_string()
}

fn parse_workflow_id(value: &str) -> Result<WorkflowId, String> {
    uuid::Uuid::parse_str(value)
        .map(WorkflowId::new)
        .map_err(|error| format!("invalid_child_workflow_id:{error}"))
}

fn decode_payload_arg(term: Term) -> Result<Payload, String> {
    decode_string_arg(term).map(|value| Payload::new(ContentType::Json, value.into_bytes()))
}

fn term_or_encoding_error(term: Option<Term>) -> Result<Term, String> {
    term.ok_or_else(|| "failed_to_encode_child_nif_result".to_owned())
}

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = Binary::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn require_arity(name: &str, args: &[Term], expected: usize) -> Result<(), String> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(format!(
            "{name}: expected {expected} arguments, got {}",
            args.len()
        ))
    }
}

fn payload_text(payload: &Payload) -> Result<String, String> {
    String::from_utf8(payload.bytes().to_vec()).map_err(|_| "payload is not valid UTF-8".to_owned())
}

fn workflow_error_text(error: &WorkflowError) -> String {
    match &error.details {
        Some(details) => payload_text(details).unwrap_or_else(|_| error.message.clone()),
        None => error.message.clone(),
    }
}

fn ok_result_term(value: &str) -> Option<Term> {
    let value_term = alloc_binary_term(value.as_bytes())?;
    alloc_tuple_term(&[Term::atom(Atom::OK), value_term])
}

fn error_result_term(message: &str) -> Option<Term> {
    let value_term = alloc_binary_term(message.as_bytes())?;
    alloc_tuple_term(&[Term::atom(Atom::ERROR), value_term])
}

fn alloc_binary_term(bytes: &[u8]) -> Option<Term> {
    let word_count = 2 + binary::packed_word_count(bytes.len());
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = binary::write_binary(&mut heap, bytes)?;
    CHILD_NIF_HEAP.with_borrow_mut(|parked| parked.push(heap));
    Some(term)
}

fn alloc_tuple_term(elements: &[Term]) -> Option<Term> {
    let word_count = 1 + elements.len();
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = boxed::write_tuple(&mut heap, elements)?;
    CHILD_NIF_HEAP.with_borrow_mut(|parked| parked.push(heap));
    Some(term)
}
