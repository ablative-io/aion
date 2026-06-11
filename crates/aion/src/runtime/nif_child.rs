//! Child-workflow NIF bridge implementations.

use std::sync::Arc;

use aion_core::{ContentType, Payload, RunId, WorkflowError, WorkflowId};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary;
use beamr::term::binary_ref::BinaryRef;
use beamr::term::boxed;

use crate::child::{ChildWorkflowError, ChildWorkflowRecordingContext, await_child, spawn};
use crate::durability::{Command, CorrelationKey, DurabilityError, Resolution, ResolveOutcome};
use crate::runtime::nif_child_engine::{ChildNifBridge, CompletionMailbox, NifChildEngine};

use super::nif_context::{NifContext, NifContextError};

thread_local! {
    static CHILD_NIF_HEAP: std::cell::RefCell<Vec<Box<[u64]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Installs the engine-owned dependencies used by child workflow NIFs.
pub(crate) fn install_child_nif_bridge(
    state: &super::nif_state::EngineNifState,
    bridge: Arc<ChildNifBridge>,
) {
    match state.child_bridge.write() {
        Ok(mut slot) => *slot = Some(bridge),
        Err(poisoned) => *poisoned.into_inner() = Some(bridge),
    }
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
    let bridge = child_bridge(ctx)?;
    let pid = ctx.pid().ok_or_else(|| "missing_caller_pid".to_owned())?;
    // spawn_child records `ChildWorkflowStarted`; a query handler must stay
    // read-only.
    let state = super::nif_state::engine_nif_state(ctx)?;
    super::nif_query_pump::ensure_not_servicing_query(&state, pid, "spawn_child")?;
    let mut nif = new_context(&bridge, pid)?;
    let key = next_child_key(&nif);
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
    let bridge = child_bridge(ctx)?;
    let pid = ctx.pid().ok_or_else(|| "missing_caller_pid".to_owned())?;
    // await_child records the awaited child terminal through the recorder; a
    // query handler must stay read-only.
    let state = super::nif_state::engine_nif_state(ctx)?;
    super::nif_query_pump::ensure_not_servicing_query(&state, pid, "await_child")?;
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

fn child_bridge(ctx: &ProcessContext) -> Result<Arc<ChildNifBridge>, String> {
    let state = super::nif_state::engine_nif_state(ctx)?;
    let slot = match state.child_bridge.read() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    slot.ok_or_else(|| "no_child_nif_bridge_configured".to_owned())
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

/// Derives the spawn's correlation key from the run-scoped child ordinal.
///
/// The ordinal counter lives on the workflow handle and restarts at zero for
/// every fresh run (live start, crash-recovery re-spawn, continue-as-new
/// replacement), so the n-th `spawn_child` call always correlates with the
/// n-th recorded `ChildWorkflowStarted` in the run segment — independent of
/// the recorder's sequence head, which moves with asynchronous-arrival
/// appends and resumes at the full history head after recovery.
fn next_child_key(nif: &NifContext) -> CorrelationKey {
    CorrelationKey::Child(nif.next_child_ordinal())
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
    let bin = BinaryRef::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore, WriteToken};
    use serde_json::json;

    use super::next_child_key;
    use crate::durability::{CorrelationKey, Recorder};
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_context::NifContext;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn started_event(workflow_id: &WorkflowId, run_id: &RunId) -> TestEvent {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: chrono::Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: "parent".to_owned(),
            input: Payload::from_json(&json!({ "fixture": "input" }))?,
            run_id: run_id.clone(),
            parent_run_id: None,
        })
    }

    type TestEvent = Result<Event, Box<dyn std::error::Error>>;

    fn registered_context(
        runtime: &tokio::runtime::Runtime,
        pid: u64,
        resume_head: u64,
    ) -> Result<(Registry, Arc<dyn EventStore>), Box<dyn std::error::Error>> {
        let registry = Registry::default();
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        runtime.block_on(store.append(
            WriteToken::recorder(),
            &workflow_id,
            &[started_event(&workflow_id, &run_id)?],
            0,
        ))?;
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), resume_head);
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "parent".to_owned(),
            loaded_version: ContentHash::from_bytes([3; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle)?;
        Ok((registry, store))
    }

    #[test]
    fn child_keys_are_run_scoped_ordinals_independent_of_recorder_head() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        // A recovered run resumes its recorder at the full history head; the
        // spawn key must still start at ordinal zero, not head + 1.
        let (registry, store) = registered_context(&runtime, 91, 57)?;
        let first_call = NifContext::new_with_history_store(
            91,
            &registry,
            runtime.handle().clone(),
            Some(Arc::clone(&store)),
        )?;
        let second_call = NifContext::new_with_history_store(
            91,
            &registry,
            runtime.handle().clone(),
            Some(store),
        )?;

        assert_eq!(next_child_key(&first_call), CorrelationKey::Child(0));
        // Distinct NIF calls share the handle-owned counter, so the second
        // spawn in the same run advances to the next ordinal.
        assert_eq!(next_child_key(&second_call), CorrelationKey::Child(1));
        Ok(())
    }
}
