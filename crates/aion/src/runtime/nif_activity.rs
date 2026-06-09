//! Durable `run_activity/3` NIF implementation.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock, RwLock};

use aion_core::{ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed;
use chrono::Utc;
use tokio::runtime::Handle;

use crate::activity::bridge::{ActivityDispatcher, activity_dispatcher};
use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome};
use crate::registry::Registry;
use crate::runtime::nif_context::{NifContext, NifContextError};

thread_local! {
    static ACTIVITY_NIF_HEAP: RefCell<Vec<Box<[u64]>>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
struct RuntimeContext {
    registry: Arc<Registry>,
    tokio_handle: Handle,
}

static RUNTIME_CONTEXT: OnceLock<RwLock<Option<RuntimeContext>>> = OnceLock::new();

pub(crate) fn install_nif_runtime_context(registry: Arc<Registry>, tokio_handle: Handle) {
    let context = RuntimeContext {
        registry,
        tokio_handle,
    };
    let cell = RUNTIME_CONTEXT.get_or_init(|| RwLock::new(None));
    if let Ok(mut slot) = cell.write() {
        *slot = Some(context);
    }
}

fn runtime_context() -> Result<RuntimeContext, NifContextError> {
    let Some(cell) = RUNTIME_CONTEXT.get() else {
        return Err(NifContextError::TermEncoding {
            reason: "nif runtime context is not installed".to_owned(),
        });
    };
    let guard = cell.read().map_err(|_| NifContextError::TermEncoding {
        reason: "nif runtime context lock is poisoned".to_owned(),
    })?;
    guard.clone().ok_or_else(|| NifContextError::TermEncoding {
        reason: "nif runtime context is not installed".to_owned(),
    })
}

fn park_heap(heap: Box<[u64]>) {
    ACTIVITY_NIF_HEAP.with_borrow_mut(|parked| parked.push(heap));
}

#[cfg(test)]
fn clear_parked_heap() {
    ACTIVITY_NIF_HEAP.with_borrow_mut(Vec::clear);
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

fn tagged_result_term(tag: Atom, bytes: &[u8]) -> Option<Term> {
    let value = alloc_binary_term(bytes)?;
    alloc_tuple_term(&[Term::atom(tag), value])
}

fn ok_result_term(bytes: &[u8]) -> Option<Term> {
    tagged_result_term(Atom::OK, bytes)
}

fn error_result_term(message: &str) -> Option<Term> {
    tagged_result_term(Atom::ERROR, message.as_bytes())
}

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = Binary::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn json_payload(text: &str, label: &str) -> Result<Payload, Term> {
    let value = serde_json::from_str(text).map_err(|error| {
        error_result_term(&format!(
            "run_activity {label}: invalid JSON payload: {error}"
        ))
        .unwrap_or(Term::NIL)
    })?;
    Payload::from_json(&value).map_err(|error| {
        error_result_term(&format!("run_activity {label}: {error}")).unwrap_or(Term::NIL)
    })
}

fn result_payload(result: &str) -> Payload {
    Payload::new(ContentType::Json, result.as_bytes().to_vec())
}

fn activity_error(reason: String) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: reason,
        details: None,
    }
}

fn context_error_term(error: &NifContextError) -> Term {
    match error.to_error_term() {
        Ok(term) => term,
        Err(_) => Term::NIL,
    }
}

fn run_recorded(resolution: Resolution) -> Term {
    match resolution {
        Resolution::ActivityCompleted(payload) => {
            ok_result_term(payload.bytes()).unwrap_or(Term::NIL)
        }
        Resolution::ActivityFailedTerminal(error) => {
            error_result_term(&error.message).unwrap_or(Term::NIL)
        }
        other => error_result_term(&format!(
            "run_activity: recorded non-activity resolution {other:?}"
        ))
        .unwrap_or(Term::NIL),
    }
}

fn record_started(
    context: &NifContext,
    activity_id: ActivityId,
    activity_type: String,
    input: Payload,
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_scheduled_started(recorded_at, activity_id, activity_type, input)
        .map_err(|error| context_error_term(&error))
}

fn record_completed(
    context: &NifContext,
    activity_id: ActivityId,
    result: Payload,
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_completed(recorded_at, activity_id, result)
        .map_err(|error| context_error_term(&error))
}

fn record_failed(
    context: &NifContext,
    activity_id: ActivityId,
    error: ActivityError,
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_failed(recorded_at, activity_id, error, 1)
        .map_err(|error| context_error_term(&error))
}

fn run_live(
    context: &NifContext,
    dispatcher: &dyn ActivityDispatcher,
    activity_id: ActivityId,
    name: &str,
    input_text: &str,
    config: &str,
    input_payload: Payload,
) -> Result<Term, Term> {
    record_started(context, activity_id.clone(), name.to_owned(), input_payload)?;
    match dispatcher.dispatch_from_process(name, input_text, config, Some(context.pid())) {
        Ok(result) => {
            record_completed(context, activity_id, result_payload(&result))?;
            Ok(ok_result_term(result.as_bytes()).unwrap_or(Term::NIL))
        }
        Err(reason) => {
            let error = activity_error(reason.clone());
            record_failed(context, activity_id, error)?;
            Ok(error_result_term(&reason).unwrap_or(Term::NIL))
        }
    }
}

fn run_activity_with_context_and_dispatcher(
    mut context: NifContext,
    dispatcher: Option<&dyn ActivityDispatcher>,
    name: &str,
    input_text: &str,
    config: &str,
) -> Result<Term, Term> {
    let input_payload = json_payload(input_text, "input")?;
    let ordinal = context.next_activity_ordinal();
    let key = CorrelationKey::Activity(ordinal);
    let activity_id = ActivityId::from_sequence_position(ordinal);
    match context
        .resolve_command(Command::RunActivity {
            key,
            activity_type: name.to_owned(),
            input: input_payload.clone(),
        })
        .map_err(|error| context_error_term(&error))?
    {
        ResolveOutcome::Recorded(resolution) => Ok(run_recorded(resolution)),
        ResolveOutcome::ResumeLive => {
            let Some(dispatcher) = dispatcher else {
                return Ok(error_result_term(
                    "no activity dispatcher configured — set one via EngineBuilder::activity_dispatcher",
                )
                .unwrap_or(Term::NIL));
            };
            run_live(
                &context,
                dispatcher,
                activity_id,
                name,
                input_text,
                config,
                input_payload,
            )
        }
    }
}

/// NIF backing `aion_flow_ffi:run_activity/3`.
pub(super) fn run_activity_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 3 {
        let msg = format!("run_activity: expected 3 arguments, got {}", args.len());
        return Ok(error_result_term(&msg).unwrap_or(Term::NIL));
    }

    let name = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("run_activity name: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let input = match decode_string_arg(args[1]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("run_activity input: {error}")).unwrap_or(Term::NIL)
            );
        }
    };
    let config = match decode_string_arg(args[2]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("run_activity config: {error}")).unwrap_or(Term::NIL)
            );
        }
    };

    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term("run_activity: missing calling process pid").unwrap_or(Term::NIL),
        );
    };
    let runtime = match runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return Ok(context_error_term(&error)),
    };
    let context = match NifContext::new(pid, runtime.registry.as_ref(), runtime.tokio_handle) {
        Ok(context) => context,
        Err(error) => return Ok(context_error_term(&error)),
    };
    let dispatcher = activity_dispatcher();

    run_activity_with_context_and_dispatcher(context, dispatcher.as_deref(), &name, &input, &config)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use aion_core::{Event, EventEnvelope, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore};
    use beamr::term::binary::Binary;
    use beamr::term::boxed::Tuple;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, WorkflowHandle, WorkflowHandleParts,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct RecordingDispatcher {
        calls: AtomicUsize,
        result: Result<String, String>,
    }

    impl ActivityDispatcher for RecordingDispatcher {
        fn dispatch(&self, name: &str, input: &str, config: &str) -> Result<String, String> {
            let _ = (name, input, config);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    struct OrderCheckingDispatcher {
        calls: AtomicUsize,
        runtime: tokio::runtime::Handle,
        store: Arc<dyn EventStore>,
        workflow_id: aion_core::WorkflowId,
        observed_pre_dispatch_events: AtomicUsize,
        result: Result<String, String>,
    }

    impl ActivityDispatcher for OrderCheckingDispatcher {
        fn dispatch(&self, name: &str, input: &str, config: &str) -> Result<String, String> {
            let _ = (name, input, config);
            let history = self
                .runtime
                .block_on(self.store.read_history(&self.workflow_id))
                .unwrap_or_default();
            self.observed_pre_dispatch_events
                .store(history.len(), Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    fn payload(value: serde_json::Value) -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(&value)?)
    }

    fn envelope(
        workflow_id: &aion_core::WorkflowId,
        seq: u64,
    ) -> Result<EventEnvelope, Box<dyn std::error::Error>> {
        Ok(EventEnvelope {
            seq,
            recorded_at: Utc
                .timestamp_opt(i64::try_from(seq)?, 0)
                .single()
                .ok_or_else(|| "invalid timestamp".to_owned())?,
            workflow_id: workflow_id.clone(),
        })
    }

    fn hash() -> ContentHash {
        ContentHash::from_bytes([3; 32])
    }

    fn context_with_history(
        runtime: &tokio::runtime::Runtime,
        pid: u64,
        workflow_id: aion_core::WorkflowId,
        history: &[Event],
    ) -> Result<(NifContext, Arc<dyn EventStore>, aion_core::WorkflowId), Box<dyn std::error::Error>>
    {
        let registry = Registry::default();
        let run_id = aion_core::RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        if !history.is_empty() {
            runtime.block_on(store.append(&workflow_id, history, 0))?;
        }
        let recorder = Recorder::resume_at(
            workflow_id.clone(),
            Arc::clone(&store),
            history.len() as u64,
        );
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "checkout".to_owned(),
            loaded_version: hash(),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id.clone(), run_id), handle)?;
        let context = NifContext::new_with_history_store(
            pid,
            &registry,
            runtime.handle().clone(),
            Some(Arc::clone(&store)),
        )?;
        Ok((context, store, workflow_id))
    }

    fn decode_result_tuple(term: Term) -> Result<(String, Vec<u8>), Box<dyn std::error::Error>> {
        let tuple = Tuple::new(term).ok_or("result should be a tuple")?;
        let tag = tuple.get(0).ok_or("missing tag")?;
        let value = tuple.get(1).ok_or("missing value")?;
        let tag_name = if tag == Term::atom(Atom::OK) {
            "ok"
        } else {
            "error"
        };
        let bin = Binary::new(value).ok_or("value should be binary")?;
        Ok((tag_name.to_owned(), bin.as_bytes().to_vec()))
    }

    #[test]
    fn replay_completed_activity_returns_recorded_result_without_dispatch() -> TestResult {
        clear_parked_heap();
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let result = payload(json!({ "ok": true }))?;
        let history = vec![
            Event::ActivityScheduled {
                envelope: envelope(&workflow_id, 1)?,
                activity_id: ActivityId::from_sequence_position(0),
                activity_type: "greet".to_owned(),
                input: payload(json!({ "name": "Ada" }))?,
            },
            Event::ActivityCompleted {
                envelope: envelope(&workflow_id, 2)?,
                activity_id: ActivityId::from_sequence_position(0),
                result: result.clone(),
            },
        ];
        let (context, _store, _workflow_id) =
            context_with_history(&runtime, 91, workflow_id, &history)?;
        let dispatcher = RecordingDispatcher {
            calls: AtomicUsize::new(0),
            result: Ok("{\"unexpected\":true}".to_owned()),
        };

        let term = run_activity_with_context_and_dispatcher(
            context,
            Some(&dispatcher),
            "greet",
            "{\"name\":\"Ada\"}",
            "{}",
        )
        .map_err(|_| "activity NIF returned a beam-level error")?;

        let (tag, bytes) = decode_result_tuple(term)?;
        assert_eq!(tag, "ok");
        assert_eq!(bytes, result.bytes());
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn live_success_records_scheduled_started_completed_around_dispatch() -> TestResult {
        clear_parked_heap();
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let (context, store, workflow_id) = context_with_history(&runtime, 92, workflow_id, &[])?;
        let dispatcher = OrderCheckingDispatcher {
            calls: AtomicUsize::new(0),
            runtime: runtime.handle().clone(),
            store: Arc::clone(&store),
            workflow_id: workflow_id.clone(),
            observed_pre_dispatch_events: AtomicUsize::new(0),
            result: Ok("plain result".to_owned()),
        };

        let term = run_activity_with_context_and_dispatcher(
            context,
            Some(&dispatcher),
            "greet",
            "{\"name\":\"Ada\"}",
            "{}",
        )
        .map_err(|_| "activity NIF returned a beam-level error")?;

        let (tag, bytes) = decode_result_tuple(term)?;
        assert_eq!(tag, "ok");
        assert_eq!(bytes, b"plain result");
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            dispatcher
                .observed_pre_dispatch_events
                .load(Ordering::SeqCst),
            2
        );
        let history = runtime.block_on(store.read_history(&workflow_id))?;
        assert!(matches!(
            history.first(),
            Some(Event::ActivityScheduled { .. })
        ));
        assert!(matches!(
            history.get(1),
            Some(Event::ActivityStarted { .. })
        ));
        assert!(matches!(
            history.get(2),
            Some(Event::ActivityCompleted { .. })
        ));
        for event in &history {
            assert_eq!(event.workflow_id(), &workflow_id);
        }
        Ok(())
    }

    #[test]
    fn live_failure_records_terminal_failed_with_same_activity_id() -> TestResult {
        clear_parked_heap();
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let (context, store, workflow_id) = context_with_history(&runtime, 93, workflow_id, &[])?;
        let dispatcher = RecordingDispatcher {
            calls: AtomicUsize::new(0),
            result: Err("boom".to_owned()),
        };

        let term = run_activity_with_context_and_dispatcher(
            context,
            Some(&dispatcher),
            "greet",
            "{\"name\":\"Ada\"}",
            "{}",
        )
        .map_err(|_| "activity NIF returned a beam-level error")?;

        let (tag, bytes) = decode_result_tuple(term)?;
        assert_eq!(tag, "error");
        assert_eq!(bytes, b"boom");
        let history = runtime.block_on(store.read_history(&workflow_id))?;
        let scheduled_id = match history.first() {
            Some(Event::ActivityScheduled { activity_id, .. }) => activity_id.clone(),
            _ => return Err("expected scheduled event".into()),
        };
        match history.get(2) {
            Some(Event::ActivityFailed {
                activity_id,
                error,
                attempt,
                ..
            }) => {
                assert_eq!(activity_id, &scheduled_id);
                assert_eq!(error.kind, ActivityErrorKind::Terminal);
                assert_eq!(error.message, "boom");
                assert_eq!(*attempt, 1);
            }
            _ => return Err("expected failed event".into()),
        }
        Ok(())
    }
}
