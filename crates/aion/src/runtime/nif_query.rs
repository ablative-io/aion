//! Query NIF bridge for `aion_flow_ffi`.
//!
//! Queries are read-only live inspections. This bridge validates the calling
//! workflow process, keeps workflow-local handler registrations, and coordinates
//! pending replies without touching the durability recorder or replay resolver.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aion_core::{ContentType, Payload, WorkflowId};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary;
use beamr::term::binary_ref::BinaryRef;
use beamr::term::boxed;
use serde::Deserialize;
use tokio::runtime::Handle;

use crate::engine_seam::{EngineHandle, QueryReplySender};
use crate::query::{QueryError, QueryService};
use crate::registry::Registry;

use super::nif_context::NifContext;
use super::nif_query_mailbox::QueryMailboxEngine;
use super::nif_state::EngineNifState;

#[cfg(test)]
#[path = "nif_query_tests.rs"]
mod nif_query_tests;

thread_local! {
    static QUERY_NIF_HEAP: std::cell::RefCell<Vec<Box<[u64]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QueryHandlerRef {
    pub(crate) pid: u64,
    pub(crate) handler: Term,
}

#[derive(Clone)]
pub(super) struct QueryBridgeState {
    registry: Arc<Registry>,
    engine: Arc<dyn EngineHandle>,
    tokio_handle: Handle,
    mailbox_engine: Arc<QueryMailboxEngine>,
}

type HandlerMap = HashMap<(u64, String), QueryHandlerRef>;
type PendingMap = HashMap<String, QueryReplySender>;

#[derive(Default)]
pub(super) struct QueryHandlers {
    handlers: Mutex<HandlerMap>,
    pending: Mutex<PendingMap>,
}

#[derive(Deserialize)]
struct DispatchConfig {
    target_workflow_id: WorkflowId,
    payload: Option<String>,
}

pub(crate) fn install_query_bridge(
    state: &Arc<EngineNifState>,
    registry: Arc<Registry>,
    tokio_handle: Handle,
) {
    let mailbox_engine = Arc::new(QueryMailboxEngine::new(
        Arc::clone(&registry),
        Arc::downgrade(state),
    ));
    install_query_bridge_state(
        state,
        registry,
        mailbox_engine.clone(),
        tokio_handle,
        mailbox_engine,
    );
}

#[cfg(test)]
fn install_query_bridge_with_engine(
    state: &Arc<EngineNifState>,
    registry: Arc<Registry>,
    engine: Arc<dyn EngineHandle>,
    tokio_handle: Handle,
) {
    let mailbox_engine = Arc::new(QueryMailboxEngine::new(
        Arc::clone(&registry),
        Arc::downgrade(state),
    ));
    install_query_bridge_state(state, registry, engine, tokio_handle, mailbox_engine);
}

fn install_query_bridge_state(
    state: &EngineNifState,
    registry: Arc<Registry>,
    engine: Arc<dyn EngineHandle>,
    tokio_handle: Handle,
    mailbox_engine: Arc<QueryMailboxEngine>,
) {
    let installed = QueryBridgeState {
        registry,
        engine,
        tokio_handle,
        mailbox_engine,
    };
    match state.query_bridge.lock() {
        Ok(mut bridge) => *bridge = Some(installed),
        Err(poisoned) => *poisoned.into_inner() = Some(installed),
    }
}

pub(crate) fn register_query_impl(
    state: &EngineNifState,
    name: &str,
    handler: Term,
    config: &str,
    caller_pid: Option<u64>,
) -> Result<String, String> {
    let context = context_for(state, caller_pid)?;
    let _ = config;
    let handler_ref = QueryHandlerRef {
        pid: context.pid(),
        handler,
    };
    state
        .query_handlers
        .lock_handlers()?
        .insert((context.pid(), name.to_owned()), handler_ref);
    Ok("registered".to_owned())
}

pub(crate) fn reply_query_impl(
    state: &EngineNifState,
    query_id: &str,
    response_payload: &str,
    caller_pid: Option<u64>,
) -> Result<String, String> {
    let context = context_for(state, caller_pid)?;
    let _ = context.pid();
    let sender = state
        .query_handlers
        .lock_pending()?
        .remove(query_id)
        .ok_or_else(|| format!("unknown_query_id:{query_id}"))?;
    let payload = payload_from_string(response_payload);
    sender
        .send(Ok(payload))
        .map_err(|_| format!("reply_dropped:{query_id}"))?;
    Ok("replied".to_owned())
}

pub(crate) fn dispatch_query_impl(
    state: &EngineNifState,
    name: &str,
    config: &str,
    caller_pid: Option<u64>,
) -> Result<String, String> {
    let context = context_for(state, caller_pid)?;
    let _ = context.pid();
    let bridge = query_bridge(state)?;
    let parsed = parse_dispatch_config(config)?;
    let payload = payload_from_string(parsed.payload.as_deref().unwrap_or("{}"));
    let engine = dispatch_engine(state, &bridge, &parsed.target_workflow_id, name)?;
    let service = QueryService::new(engine, Duration::from_secs(30));
    let result =
        bridge
            .tokio_handle
            .block_on(service.query(&parsed.target_workflow_id, name, payload));
    result
        .map(|p| payload_to_string(&p))
        .map_err(|error| query_error_reason(&error))
}

pub(crate) fn registered_handler(
    state: &EngineNifState,
    pid: u64,
    name: &str,
) -> Result<Option<QueryHandlerRef>, String> {
    Ok(state
        .query_handlers
        .lock_handlers()?
        .get(&(pid, name.to_owned()))
        .copied())
}

fn dispatch_engine(
    state: &EngineNifState,
    bridge: &QueryBridgeState,
    workflow_id: &WorkflowId,
    name: &str,
) -> Result<Arc<dyn EngineHandle>, String> {
    let process = bridge
        .registry
        .list()
        .map_err(|error| format!("registry:{error}"))?
        .into_iter()
        .find(|handle| handle.workflow_id() == workflow_id)
        .map(|handle| handle.pid());
    if let Some(pid) = process {
        if registered_handler(state, pid, name)?.is_some() {
            return Ok(bridge.mailbox_engine.clone());
        }
    }
    Ok(Arc::clone(&bridge.engine))
}

pub(crate) fn register_query(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 3 {
        let message = format!("register_query: expected 3 arguments, got {}", args.len());
        return Ok(error_result_term(&message).unwrap_or(Term::NIL));
    }
    let name = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("register_query name: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let config = match decode_string_arg(args[2]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("register_query config: {error}")).unwrap_or(Term::NIL),
            );
        }
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    match register_query_impl(&state, &name, args[1], &config, ctx.pid()) {
        Ok(value) => Ok(ok_result_term(&value).unwrap_or(Term::NIL)),
        Err(error) => Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    }
}

pub(crate) fn reply_query(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        let message = format!("reply_query: expected 2 arguments, got {}", args.len());
        return Ok(error_result_term(&message).unwrap_or(Term::NIL));
    }
    let query_id = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(error_result_term(&format!("reply_query id: {error}")).unwrap_or(Term::NIL));
        }
    };
    let response = match decode_string_arg(args[1]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("reply_query response: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    match reply_query_impl(&state, &query_id, &response, ctx.pid()) {
        Ok(value) => Ok(ok_result_term(&value).unwrap_or(Term::NIL)),
        Err(error) => Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    }
}

pub(crate) fn dispatch_query(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        let message = format!("dispatch_query: expected 2 arguments, got {}", args.len());
        return Ok(error_result_term(&message).unwrap_or(Term::NIL));
    }
    let name = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("dispatch_query name: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let config = match decode_string_arg(args[1]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("dispatch_query config: {error}")).unwrap_or(Term::NIL),
            );
        }
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    match dispatch_query_impl(&state, &name, &config, ctx.pid()) {
        Ok(value) => Ok(ok_result_term(&value).unwrap_or(Term::NIL)),
        Err(error) => Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    }
}

fn context_for(state: &EngineNifState, caller_pid: Option<u64>) -> Result<NifContext, String> {
    let pid = caller_pid.ok_or_else(|| "missing_process_pid".to_owned())?;
    let bridge = query_bridge(state)?;
    NifContext::new(pid, bridge.registry.as_ref(), bridge.tokio_handle.clone())
        .map_err(|error| error.to_string())
}

fn query_bridge(state: &EngineNifState) -> Result<QueryBridgeState, String> {
    state
        .query_bridge
        .lock()
        .map_err(|_| "query bridge lock poisoned".to_owned())?
        .clone()
        .ok_or_else(|| "no query bridge configured".to_owned())
}

/// Insert a pending reply sender for `query_id`, used while a query awaits
/// its workflow-side `reply_query` call.
#[cfg(test)]
fn insert_pending_reply(
    state: &EngineNifState,
    query_id: String,
    sender: QueryReplySender,
) -> Result<(), String> {
    state
        .query_handlers
        .lock_pending()?
        .insert(query_id, sender);
    Ok(())
}

impl QueryHandlers {
    fn lock_handlers(&self) -> Result<MutexGuard<'_, HandlerMap>, String> {
        self.handlers
            .lock()
            .map_err(|_| "query handler registry lock poisoned".to_owned())
    }

    fn lock_pending(&self) -> Result<MutexGuard<'_, PendingMap>, String> {
        self.pending
            .lock()
            .map_err(|_| "pending query registry lock poisoned".to_owned())
    }
}

fn parse_dispatch_config(config: &str) -> Result<DispatchConfig, String> {
    serde_json::from_str(config).map_err(|error| format!("invalid_query_config:{error}"))
}

pub(super) fn payload_from_string(value: &str) -> Payload {
    Payload::new(ContentType::Json, value.as_bytes().to_vec())
}

fn payload_to_string(payload: &Payload) -> String {
    String::from_utf8(payload.bytes().to_vec()).unwrap_or_else(|error| {
        let bytes = error.into_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn query_error_reason(error: &QueryError) -> String {
    match error {
        QueryError::UnknownQuery(name) => format!("unknown:{name}"),
        QueryError::Timeout => "timeout".to_owned(),
        QueryError::NotRunning(workflow_id) => format!("not_running:{workflow_id}"),
        QueryError::Unknown(workflow_id) => format!("unknown_workflow:{workflow_id}"),
        QueryError::ReplyDropped => "reply_dropped".to_owned(),
        QueryError::Engine(error) => format!("engine:{error}"),
    }
}

fn park_heap(heap: Box<[u64]>) {
    QUERY_NIF_HEAP.with_borrow_mut(|parked| parked.push(heap));
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
