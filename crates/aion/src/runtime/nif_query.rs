//! Query NIF bridge for `aion_flow_ffi`.
//!
//! Queries are read-only live inspections. This bridge validates the calling
//! workflow process, keeps workflow-local handler-name registrations, and
//! coordinates pending replies without touching the durability recorder or
//! replay resolver. Handler funs are never stored on the Rust side: beamr's
//! moving GC rewrites roots in place, so a Term held in a Rust map dangles
//! after the first workflow-process GC. Handlers live in the workflow
//! process dictionary, written by the SDK at registration.

use std::collections::{HashMap, HashSet};
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
use crate::runtime::RuntimeHandle;

use super::nif_context::NifContext;
use super::nif_query_mailbox::QueryMailboxEngine;
use super::nif_query_pump::{clear_servicing_query, ensure_not_servicing_query, is_mid_replay};
use super::nif_state::EngineNifState;

#[cfg(test)]
#[path = "nif_query_tests.rs"]
mod nif_query_tests;

thread_local! {
    static QUERY_NIF_HEAP: std::cell::RefCell<Vec<Box<[u64]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Clone)]
pub(super) struct QueryBridgeState {
    registry: Arc<Registry>,
    engine: Arc<dyn EngineHandle>,
    tokio_handle: Handle,
    mailbox_engine: Arc<QueryMailboxEngine>,
    /// Engine-configured query timeout for in-engine `dispatch_query` calls.
    /// `None` means the engine was built without `EngineBuilder::query_timeout`
    /// and dispatching fails typed — never a hardcoded fallback.
    query_timeout: Option<Duration>,
}

/// Names registered as queryable, keyed by workflow pid.
type HandlerSet = HashSet<(u64, String)>;

/// A query reply channel pending its workflow-side `reply_query` call.
pub(super) struct PendingReply {
    /// Workflow pid the query was delivered to, for exit-time cleanup.
    pub(super) pid: u64,
    /// One-shot sender back to the waiting `QueryService` caller.
    pub(super) sender: QueryReplySender,
}

type PendingMap = HashMap<String, PendingReply>;

#[derive(Default)]
pub(super) struct QueryHandlers {
    handlers: Mutex<HandlerSet>,
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
    runtime: &Arc<RuntimeHandle>,
    tokio_handle: Handle,
    query_timeout: Option<Duration>,
) -> Arc<dyn EngineHandle> {
    let mailbox_engine = Arc::new(QueryMailboxEngine::new(
        Arc::clone(&registry),
        Arc::downgrade(state),
        Arc::downgrade(runtime),
    ));
    install_query_bridge_state(
        state,
        registry,
        mailbox_engine.clone(),
        tokio_handle,
        mailbox_engine.clone(),
        query_timeout,
    );
    mailbox_engine
}

#[cfg(test)]
pub(super) struct TestQueryBridgeParts {
    pub(super) registry: Arc<Registry>,
    pub(super) engine: Arc<dyn EngineHandle>,
    pub(super) runtime: std::sync::Weak<RuntimeHandle>,
    pub(super) tokio_handle: Handle,
    pub(super) query_timeout: Option<Duration>,
}

#[cfg(test)]
fn install_query_bridge_with_engine(state: &Arc<EngineNifState>, parts: TestQueryBridgeParts) {
    let mailbox_engine = Arc::new(QueryMailboxEngine::new(
        Arc::clone(&parts.registry),
        Arc::downgrade(state),
        parts.runtime,
    ));
    install_query_bridge_state(
        state,
        parts.registry,
        parts.engine,
        parts.tokio_handle,
        mailbox_engine,
        parts.query_timeout,
    );
}

fn install_query_bridge_state(
    state: &EngineNifState,
    registry: Arc<Registry>,
    engine: Arc<dyn EngineHandle>,
    tokio_handle: Handle,
    mailbox_engine: Arc<QueryMailboxEngine>,
    query_timeout: Option<Duration>,
) {
    let installed = QueryBridgeState {
        registry,
        engine,
        tokio_handle,
        mailbox_engine,
        query_timeout,
    };
    match state.query_bridge.lock() {
        Ok(mut bridge) => *bridge = Some(installed),
        Err(poisoned) => *poisoned.into_inner() = Some(installed),
    }
}

pub(crate) fn register_query_impl(
    state: &EngineNifState,
    name: &str,
    config: &str,
    caller_pid: Option<u64>,
) -> Result<String, String> {
    let context = context_for(state, caller_pid)?;
    let _ = config;
    state
        .query_handlers
        .lock_handlers()?
        .insert((context.pid(), name.to_owned()));
    Ok("registered".to_owned())
}

pub(crate) fn reply_query_impl(
    state: &EngineNifState,
    query_id: &str,
    response_payload: &str,
    caller_pid: Option<u64>,
) -> Result<String, String> {
    let context = context_for(state, caller_pid)?;
    // The servicing guard lifts even when the reply itself fails below: a
    // late reply after caller timeout must not leave the workflow refusing
    // every recording NIF forever.
    clear_servicing_query(state, context.pid(), query_id);
    let pending = state
        .query_handlers
        .lock_pending()?
        .remove(query_id)
        .ok_or_else(|| format!("unknown_query_id:{query_id}"))?;
    let payload = payload_from_string(response_payload);
    pending
        .sender
        .send(Ok(payload))
        .map_err(|_| format!("reply_dropped:{query_id}"))?;
    Ok("replied".to_owned())
}

pub(crate) fn reply_query_error_impl(
    state: &EngineNifState,
    query_id: &str,
    message: &str,
    caller_pid: Option<u64>,
) -> Result<String, String> {
    let context = context_for(state, caller_pid)?;
    clear_servicing_query(state, context.pid(), query_id);
    let pending = state
        .query_handlers
        .lock_pending()?
        .remove(query_id)
        .ok_or_else(|| format!("unknown_query_id:{query_id}"))?;
    pending
        .sender
        .send(Err(QueryError::HandlerFailed {
            message: message.to_owned(),
        }))
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
    // dispatch_query is a live, nondeterministic read: it must never run
    // from a query handler (no recording, but the same misuse class) and
    // never during replay, where its answer would diverge from the
    // original execution.
    ensure_not_servicing_query(state, context.pid(), "dispatch_query")?;
    if is_mid_replay(&context) {
        return Err(
            "replay_nondeterministic:dispatch_query is a live read and cannot run during replay"
                .to_owned(),
        );
    }
    let bridge = query_bridge(state)?;
    let Some(query_timeout) = bridge.query_timeout else {
        return Err(
            "query_timeout_not_configured:set EngineBuilder::query_timeout to enable dispatch_query"
                .to_owned(),
        );
    };
    let parsed = parse_dispatch_config(config)?;
    let payload = payload_from_string(parsed.payload.as_deref().unwrap_or("{}"));
    let engine = dispatch_engine(state, &bridge, &parsed.target_workflow_id, name)?;
    let service = QueryService::new(engine, query_timeout);
    let result =
        bridge
            .tokio_handle
            .block_on(service.query(&parsed.target_workflow_id, name, payload));
    result
        .map(|p| payload_to_string(&p))
        .map_err(|error| query_error_reason(&error))
}

/// Whether `name` is registered as queryable for workflow pid `pid`.
pub(crate) fn is_query_registered(
    state: &EngineNifState,
    pid: u64,
    name: &str,
) -> Result<bool, String> {
    Ok(state
        .query_handlers
        .lock_handlers()?
        .contains(&(pid, name.to_owned())))
}

/// Insert a pending reply sender for `query_id` on behalf of `pid`.
pub(super) fn insert_pending_reply(
    state: &EngineNifState,
    query_id: String,
    pid: u64,
    sender: QueryReplySender,
) -> Result<(), String> {
    state
        .query_handlers
        .lock_pending()?
        .insert(query_id, PendingReply { pid, sender });
    Ok(())
}

/// Remove and return the pending reply for `query_id`, if still present.
pub(super) fn take_pending_reply(
    state: &EngineNifState,
    query_id: &str,
) -> Result<Option<PendingReply>, String> {
    Ok(state.query_handlers.lock_pending()?.remove(query_id))
}

/// Whether a live (caller still waiting) pending reply exists for `query_id`.
pub(super) fn pending_reply_is_live(
    state: &EngineNifState,
    query_id: &str,
) -> Result<bool, String> {
    Ok(state
        .query_handlers
        .lock_pending()?
        .get(query_id)
        .is_some_and(|pending| !pending.sender.is_closed()))
}

/// Drop pending replies whose caller already stopped waiting (timed out).
///
/// Best-effort hygiene run on every delivery so a never-woken workflow does
/// not accumulate stale senders; the matching `pending_queries` entries are
/// skipped by the pump entry check when their reply is gone.
pub(super) fn prune_closed_pending_replies(state: &EngineNifState) -> Result<(), String> {
    state
        .query_handlers
        .lock_pending()?
        .retain(|_, pending| !pending.sender.is_closed());
    Ok(())
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
        if is_query_registered(state, pid, name)? {
            return Ok(bridge.mailbox_engine.clone());
        }
    }
    Ok(Arc::clone(&bridge.engine))
}

pub(crate) fn register_query(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        let message = format!("register_query: expected 2 arguments, got {}", args.len());
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
    let config = match decode_string_arg(args[1]) {
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
    match register_query_impl(&state, &name, &config, ctx.pid()) {
        Ok(value) => Ok(ok_result_term(&value).unwrap_or(Term::NIL)),
        Err(error) => Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    }
}

/// Decode the shared `(query_id, text)` argument shape of the reply NIFs.
fn decode_reply_args(name: &str, args: &[Term]) -> Result<(String, String), String> {
    if args.len() != 2 {
        return Err(format!("{name}: expected 2 arguments, got {}", args.len()));
    }
    let query_id = decode_string_arg(args[0]).map_err(|error| format!("{name} id: {error}"))?;
    let text = decode_string_arg(args[1]).map_err(|error| format!("{name} payload: {error}"))?;
    Ok((query_id, text))
}

pub(crate) fn reply_query(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    let (query_id, response) = match decode_reply_args("reply_query", args) {
        Ok(parts) => parts,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
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

pub(crate) fn reply_query_error(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    let (query_id, message) = match decode_reply_args("reply_query_error", args) {
        Ok(parts) => parts,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    match reply_query_error_impl(&state, &query_id, &message, ctx.pid()) {
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

impl QueryHandlers {
    fn lock_handlers(&self) -> Result<MutexGuard<'_, HandlerSet>, String> {
        self.handlers
            .lock()
            .map_err(|_| "query handler registry lock poisoned".to_owned())
    }

    fn lock_pending(&self) -> Result<MutexGuard<'_, PendingMap>, String> {
        self.pending
            .lock()
            .map_err(|_| "pending query registry lock poisoned".to_owned())
    }

    /// Drop the pid's handler-name registrations and pending reply senders.
    ///
    /// Dropping a pending sender makes the waiting caller observe
    /// `ReplyDropped`. Lock poison is absorbed: this runs on the exit-monitor
    /// path, where cleanup must proceed regardless of a panicked writer.
    pub(super) fn cleanup_pid(&self, pid: u64) {
        let mut handlers = match self.handlers.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        handlers.retain(|(handler_pid, _)| *handler_pid != pid);
        drop(handlers);
        let mut pending = match self.pending.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        pending.retain(|_, reply| reply.pid != pid);
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
        QueryError::HandlerFailed { message } => format!("handler_failed:{message}"),
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
