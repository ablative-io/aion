//! Child-workflow NIF bridge implementations.
//!
//! These functions keep the raw BEAM boundary thin: decode fixed FFI terms, build a per-call
//! [`NifContext`], ask the durability resolver before live work, then delegate child mechanics to
//! `crate::child` services.

use std::sync::{Arc, OnceLock};

use aion_core::{ContentType, Event, Payload, RunId, WorkflowError, WorkflowId};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed;
use chrono::Utc;
use tokio::runtime::Handle;

use crate::child::{ChildWorkflowError, ChildWorkflowRecordingContext, await_child, spawn};
use crate::durability::{Command, CorrelationKey, DurabilityError, Resolution, ResolveOutcome};
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::lifecycle::{StartWorkflowContext, start_workflow};
use crate::loader::LoadedWorkflows;
use crate::registry::{HandleResidency, Registry, TerminalOutcome, WorkflowHandle};
use crate::runtime::RuntimeHandle;
use crate::signal::SignalResumeHandoff;
use crate::supervision::SupervisionTree;

use super::nif_context::{NifContext, NifContextError};

thread_local! {
    static CHILD_NIF_HEAP: std::cell::RefCell<Vec<Box<[u64]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

static CHILD_BRIDGE: OnceLock<Arc<ChildNifBridge>> = OnceLock::new();

/// Installs the engine-owned dependencies used by child workflow NIFs.
pub(crate) fn install_child_nif_bridge(bridge: Arc<ChildNifBridge>) {
    let _ = CHILD_BRIDGE.set(bridge);
}

/// Engine-owned context for child workflow NIF calls.
pub(crate) struct ChildNifBridge {
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    runtime: Arc<RuntimeHandle>,
    loaded_workflows: LoadedWorkflows,
    registry: Arc<Registry>,
    supervision: Arc<SupervisionTree>,
    signal_handoff: Arc<SignalResumeHandoff>,
    tokio_handle: Handle,
}

impl ChildNifBridge {
    /// Creates a bridge from engine components.
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn EventStore>,
        visibility_store: Arc<dyn VisibilityStore>,
        runtime: Arc<RuntimeHandle>,
        loaded_workflows: LoadedWorkflows,
        registry: Arc<Registry>,
        supervision: Arc<SupervisionTree>,
        signal_handoff: Arc<SignalResumeHandoff>,
        tokio_handle: Handle,
    ) -> Self {
        Self {
            store,
            visibility_store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
            signal_handoff,
            tokio_handle,
        }
    }
}

/// NIF backing `aion_flow_ffi:spawn_child/3`.
pub(super) fn spawn_child_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    checked_child_result(run_spawn_child(args, ctx), "spawn_child")
}

/// NIF backing `aion_flow_ffi:await_child/1`.
pub(super) fn await_child_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    checked_child_result(run_await_child(args, ctx), "await_child")
}

fn run_spawn_child(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, String> {
    require_arity("spawn_child", args, 3)?;
    let workflow_type = decode_string_arg(args[0]).map_err(|error| format!("workflow_type:{error}"))?;
    let input = decode_payload_arg(args[1]).map_err(|error| format!("input:{error}"))?;
    let _options = decode_string_arg(args[2]).map_err(|error| format!("options:{error}"))?;
    let bridge = child_bridge()?;
    let pid = ctx.pid().ok_or_else(|| "missing_caller_pid".to_owned())?;
    let mut nif = new_context(&bridge, pid)?;
    let key = next_child_key(&nif)?;
    let command = Command::SpawnChild {
        key,
        workflow_type: workflow_type.clone(),
        input: input.clone(),
    };

    match nif.resolve_command(command).map_err(context_error)? {
        ResolveOutcome::Recorded(Resolution::ChildStarted(child_id)) => ok_result_term(&child_id.to_string()),
        ResolveOutcome::Recorded(other) => Err(format!("unexpected_child_spawn_resolution:{other:?}")),
        ResolveOutcome::ResumeLive => {
            let engine = NifChildEngine::new(Arc::clone(&bridge), nif.workflow_handle().clone());
            let mut recording = recording_context(&nif)?;
            let child = spawn(&engine, &mut recording, workflow_type, input, RunId::new_v4())
                .map_err(child_error)?;
            ok_result_term(&child.child_workflow_id.to_string())
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

    match nif.resolve_command(command).map_err(context_error)? {
        ResolveOutcome::Recorded(Resolution::ChildCompleted(result)) => {
            ok_result_term(&prefixed_payload("ok:", &result)?)
        }
        ResolveOutcome::Recorded(Resolution::ChildFailed(error)) => {
            ok_result_term(&prefixed_error("error:", &error))
        }
        ResolveOutcome::Recorded(other) => Err(format!("unexpected_child_await_resolution:{other:?}")),
        ResolveOutcome::ResumeLive => {
            let engine = NifChildEngine::new(Arc::clone(&bridge), nif.workflow_handle().clone());
            let mut recording = recording_context(&nif)?;
            let mut mailbox = CompletionMailbox::new(&bridge, &child_workflow_id)?;
            match await_child(&engine, &mut recording, &mut mailbox, &child_workflow_id) {
                Ok(result) => ok_result_term(&prefixed_payload("ok:", &result)?),
                Err(ChildWorkflowError::Failed { error, .. }) => ok_result_term(&prefixed_error("error:", &error)),
                Err(error) => Err(child_error(error)),
            }
        }
    }
}

fn checked_child_result(result: Result<Term, String>, name: &str) -> Result<Term, Term> {
    match result {
        Ok(term) => Ok(term),
        Err(message) => Ok(error_result_term(&format!("{name}:{message}")).unwrap_or(Term::NIL)),
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
        bridge.registry.as_ref(),
        bridge.tokio_handle.clone(),
        Some(Arc::clone(&bridge.store)),
    )
    .map_err(context_error)
}

fn next_child_key(nif: &NifContext) -> Result<CorrelationKey, String> {
    let next_seq = nif
        .block_on_recorder(|recorder| Box::pin(async move { next_sequence(recorder.current_head()) }))
        .map_err(context_error)?;
    Ok(CorrelationKey::Child(next_seq))
}

fn recording_context(nif: &NifContext) -> Result<ChildWorkflowRecordingContext, String> {
    let next_seq = nif
        .block_on_recorder(|recorder| Box::pin(async move { next_sequence(recorder.current_head()) }))
        .map_err(context_error)?;
    Ok(ChildWorkflowRecordingContext::new(
        nif.workflow_id().clone(),
        next_seq,
        Utc::now(),
    ))
}

fn next_sequence(head: u64) -> Result<u64, DurabilityError> {
    head.checked_add(1).ok_or_else(|| DurabilityError::HistoryShape {
        reason: format!("child NIF sequence overflow advancing {head} by 1"),
    })
}

fn context_error(error: NifContextError) -> String {
    error.to_string()
}

fn child_error(error: ChildWorkflowError) -> String {
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

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = Binary::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn require_arity(name: &str, args: &[Term], expected: usize) -> Result<(), String> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(format!("{name}: expected {expected} arguments, got {}", args.len()))
    }
}

fn prefixed_payload(prefix: &str, payload: &Payload) -> Result<String, String> {
    let body = String::from_utf8(payload.bytes().to_vec())
        .map_err(|_| "payload is not valid UTF-8".to_owned())?;
    Ok(format!("{prefix}{body}"))
}

fn prefixed_error(prefix: &str, error: &WorkflowError) -> String {
    match &error.details {
        Some(details) => prefixed_payload(prefix, details).unwrap_or_else(|_| format!("{prefix}{}", error.message)),
        None => format!("{prefix}{}", error.message),
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

struct CompletionMailbox {
    message: Option<WorkflowMailboxMessage>,
}

impl CompletionMailbox {
    fn new(bridge: &ChildNifBridge, child_workflow_id: &WorkflowId) -> Result<Self, String> {
        let child = bridge
            .registry
            .list()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|handle| handle.workflow_id() == child_workflow_id)
            .ok_or_else(|| format!("unknown_child_workflow:{child_workflow_id}"))?;
        let mut receiver = child.completion().subscribe();
        let outcome = bridge
            .tokio_handle
            .block_on(async {
                loop {
                    if let Some(outcome) = receiver.borrow().clone() {
                        break Ok(outcome);
                    }
                    if receiver.changed().await.is_err() {
                        break Err("child_completion_channel_closed".to_owned());
                    }
                }
            })?;
        Ok(Self {
            message: Some(outcome_to_message(child_workflow_id.clone(), outcome)?),
        })
    }
}

impl crate::child::ChildWorkflowMailbox for CompletionMailbox {
    fn receive_child_workflow_message(
        &mut self,
        child_workflow_id: &WorkflowId,
    ) -> Result<WorkflowMailboxMessage, ChildWorkflowError> {
        match self.message.take() {
            Some(message) => Ok(message),
            None => Err(ChildWorkflowError::MailboxClosed {
                child_workflow_id: child_workflow_id.clone(),
            }),
        }
    }
}

fn outcome_to_message(
    child_workflow_id: WorkflowId,
    outcome: TerminalOutcome,
) -> Result<WorkflowMailboxMessage, String> {
    match outcome {
        TerminalOutcome::Completed(result) => Ok(WorkflowMailboxMessage::ChildWorkflowCompleted {
            child_workflow_id,
            correlation: 0,
            result,
        }),
        TerminalOutcome::Failed(error) => Ok(WorkflowMailboxMessage::ChildWorkflowFailed {
            child_workflow_id,
            correlation: 0,
            error,
        }),
        TerminalOutcome::Cancelled(reason) => Ok(WorkflowMailboxMessage::ChildWorkflowFailed {
            child_workflow_id,
            correlation: 0,
            error: WorkflowError {
                message: format!("cancelled:{reason}"),
                details: None,
            },
        }),
        TerminalOutcome::TimedOut(timeout) => Ok(WorkflowMailboxMessage::ChildWorkflowFailed {
            child_workflow_id,
            correlation: 0,
            error: WorkflowError {
                message: format!("timed_out:{timeout}"),
                details: None,
            },
        }),
        TerminalOutcome::ContinuedAsNew { .. } => Err("child_continued_as_new_without_terminal_result".to_owned()),
    }
}

struct NifChildEngine {
    bridge: Arc<ChildNifBridge>,
    parent: WorkflowHandle,
}

impl NifChildEngine {
    fn new(bridge: Arc<ChildNifBridge>, parent: WorkflowHandle) -> Self {
        Self { bridge, parent }
    }
}

impl EngineHandle for NifChildEngine {
    fn resolve_workflow(&self, workflow_id: &WorkflowId) -> Result<WorkflowResidency, EngineSeamError> {
        let handle = self
            .bridge
            .registry
            .list()
            .map_err(|error| EngineSeamError::Delivery { reason: error.to_string() })?
            .into_iter()
            .find(|handle| handle.workflow_id() == workflow_id);
        match handle {
            Some(handle) if handle.residency() == HandleResidency::Resident => {
                Ok(WorkflowResidency::Resident(WorkflowProcessHandle::new(handle.pid())))
            }
            Some(_) => Ok(WorkflowResidency::NonResident),
            None => Ok(WorkflowResidency::Unknown),
        }
    }

    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError> {
        match message {
            WorkflowMailboxMessage::SignalReceived { name, payload } => self
                .bridge
                .runtime
                .deliver_signal_received(process.pid(), name, payload)
                .map_err(|error| EngineSeamError::Delivery { reason: error.to_string() }),
            other => Err(EngineSeamError::Delivery { reason: format!("unsupported child NIF message: {other:?}") }),
        }
    }

    fn spawn_child_workflow(
        &self,
        request: ChildWorkflowSpawnRequest,
    ) -> Result<ChildWorkflowSpawnResult, EngineSeamError> {
        let child = self
            .bridge
            .tokio_handle
            .block_on(start_workflow(
                StartWorkflowContext {
                    store: Arc::clone(&self.bridge.store),
                    visibility_store: Arc::clone(&self.bridge.visibility_store),
                    loaded_workflows: &self.bridge.loaded_workflows,
                    runtime: self.bridge.runtime.as_ref(),
                    supervision: self.bridge.supervision.as_ref(),
                    registry: self.bridge.registry.as_ref(),
                    signal_handoff: Some(Arc::clone(&self.bridge.signal_handoff)),
                },
                &request.workflow_type,
                request.input,
            ))
            .map_err(|error| EngineSeamError::ChildSpawn { reason: error.to_string() })?;
        Ok(ChildWorkflowSpawnResult {
            child_workflow_id: child.workflow_id().clone(),
            child_process: WorkflowProcessHandle::new(child.pid()),
        })
    }

    fn terminate_linked_child_workflow(
        &self,
        _parent_workflow_id: &WorkflowId,
        child_process: WorkflowProcessHandle,
        _correlation: u64,
    ) -> Result<(), EngineSeamError> {
        self.bridge
            .runtime
            .cancel_pid(child_process.pid())
            .map_err(|error| EngineSeamError::ChildTermination { reason: error.to_string() })
    }

    fn terminate_linked_activity(
        &self,
        _parent_workflow_id: &WorkflowId,
        activity_process: crate::Pid,
        _correlation: u64,
    ) -> Result<(), EngineSeamError> {
        self.bridge
            .runtime
            .cancel_pid(activity_process)
            .map_err(|error| EngineSeamError::ChildTermination { reason: error.to_string() })
    }

    fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError> {
        let _ = entry;
        Err(EngineSeamError::TimerWheel { reason: "child NIF engine cannot arm timers".to_owned() })
    }

    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &aion_core::TimerId,
    ) -> Result<(), EngineSeamError> {
        let _ = (process, timer_id);
        Err(EngineSeamError::TimerWheel { reason: "child NIF engine cannot disarm timers".to_owned() })
    }

    fn record_workflow_event(
        &self,
        workflow_id: &WorkflowId,
        event: Event,
    ) -> Result<(), EngineSeamError> {
        if workflow_id != self.parent.workflow_id() {
            return Err(EngineSeamError::Recorder { reason: format!("cannot record child event for unrelated workflow {workflow_id}") });
        }
        record_child_event(&self.bridge.tokio_handle, &self.parent, event)
    }
}

fn record_child_event(
    tokio_handle: &Handle,
    parent: &WorkflowHandle,
    event: Event,
) -> Result<(), EngineSeamError> {
    let recorder = parent.recorder();
    tokio_handle
        .block_on(async {
            let mut recorder = recorder.lock().await;
            match event {
                Event::ChildWorkflowStarted {
                    child_workflow_id,
                    workflow_type,
                    input,
                    envelope,
                } => recorder
                    .record_child_workflow_started(envelope.recorded_at, child_workflow_id, workflow_type, input)
                    .await,
                Event::ChildWorkflowCompleted {
                    child_workflow_id,
                    result,
                    envelope,
                } => recorder
                    .record_child_workflow_completed(envelope.recorded_at, child_workflow_id, result)
                    .await,
                Event::ChildWorkflowFailed {
                    child_workflow_id,
                    error,
                    envelope,
                } => recorder
                    .record_child_workflow_failed(envelope.recorded_at, child_workflow_id, error)
                    .await,
                other => Err(DurabilityError::HistoryShape { reason: format!("child NIF cannot record non-child event: {other:?}") }),
            }
        })
        .map_err(|error| EngineSeamError::Recorder { reason: error.to_string() })
}
