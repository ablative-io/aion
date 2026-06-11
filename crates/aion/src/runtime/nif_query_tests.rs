use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aion_core::{Event, WorkflowId, WorkflowStatus};
use aion_package::ContentHash;
use aion_store::{InMemoryStore, ReadableEventStore, WritableEventStore, WriteToken};

use super::*;
use crate::Pid;
use crate::durability::Recorder;
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineSeamError, TimerWheelEntry,
    WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::query::QueryResult;
use crate::registry::{CompletionNotifier, HandleResidency, WorkflowHandle, WorkflowHandleParts};
use crate::runtime::config::RuntimeConfig;
use crate::runtime::handle::RuntimeInput;
use crate::runtime::nif_state::PendingQuery;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type TestContext = (
    Arc<EngineNifState>,
    Arc<InMemoryStore>,
    Arc<FakeEngine>,
    WorkflowId,
);

const TEST_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct FakeEngine {
    residency: Mutex<HashMap<WorkflowId, WorkflowResidency>>,
    workflows: Mutex<HashMap<u64, HashMap<String, QueryResult>>>,
}

impl FakeEngine {
    fn set_workflow(
        &self,
        workflow_id: WorkflowId,
        pid: u64,
        handlers: HashMap<String, QueryResult>,
    ) -> Result<(), EngineSeamError> {
        self.residency
            .lock()
            .map_err(|_| EngineSeamError::Delivery {
                reason: "poisoned".to_owned(),
            })?
            .insert(
                workflow_id,
                WorkflowResidency::Resident(WorkflowProcessHandle::new(pid)),
            );
        self.workflows
            .lock()
            .map_err(|_| EngineSeamError::Delivery {
                reason: "poisoned".to_owned(),
            })?
            .insert(pid, handlers);
        Ok(())
    }
}

impl EngineHandle for FakeEngine {
    fn resolve_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowResidency, EngineSeamError> {
        Ok(self
            .residency
            .lock()
            .map_err(|_| EngineSeamError::Delivery {
                reason: "poisoned".to_owned(),
            })?
            .get(workflow_id)
            .copied()
            .unwrap_or(WorkflowResidency::Unknown))
    }

    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError> {
        let WorkflowMailboxMessage::Query { name, reply_to, .. } = message else {
            return Err(EngineSeamError::Delivery {
                reason: "query only".to_owned(),
            });
        };
        let result = self
            .workflows
            .lock()
            .map_err(|_| EngineSeamError::Delivery {
                reason: "poisoned".to_owned(),
            })?
            .get(&process.pid())
            .and_then(|handlers| handlers.get(&name).cloned())
            .unwrap_or(Err(QueryError::UnknownQuery(name)));
        reply_to
            .send(result)
            .map_err(|_| EngineSeamError::Delivery {
                reason: "reply dropped".to_owned(),
            })
    }

    fn spawn_child_workflow(
        &self,
        request: ChildWorkflowSpawnRequest,
    ) -> Result<ChildWorkflowSpawnResult, EngineSeamError> {
        Err(EngineSeamError::ChildSpawn {
            reason: request.workflow_type,
        })
    }

    fn terminate_linked_child_workflow(
        &self,
        parent_workflow_id: &WorkflowId,
        child_process: WorkflowProcessHandle,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::ChildTermination {
            reason: format!("{parent_workflow_id}:{child_process:?}:{correlation}"),
        })
    }

    fn terminate_linked_activity(
        &self,
        parent_workflow_id: &WorkflowId,
        activity_process: Pid,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::ChildTermination {
            reason: format!("{parent_workflow_id}:{activity_process}:{correlation}"),
        })
    }

    fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::TimerWheel {
            reason: entry.timer_id.to_string(),
        })
    }

    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &aion_core::TimerId,
    ) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::TimerWheel {
            reason: format!("{process:?}:{timer_id}"),
        })
    }

    fn record_workflow_event(
        &self,
        workflow_id: &WorkflowId,
        event: Event,
    ) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::Recorder {
            reason: format!(
                "queries must not record event {} for {workflow_id}",
                event.seq()
            ),
        })
    }
}

fn hash() -> ContentHash {
    ContentHash::from_bytes([9; 32])
}

fn seed_started_workflow(
    runtime: &tokio::runtime::Runtime,
    registry: &Registry,
    store: &Arc<InMemoryStore>,
    pid: u64,
) -> Result<WorkflowId, Box<dyn std::error::Error>> {
    let workflow_id = WorkflowId::new_v4();
    let run_id = aion_core::RunId::new_v4();
    let started = Event::WorkflowStarted {
        envelope: aion_core::EventEnvelope {
            seq: 1,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: "checkout".to_owned(),
        input: aion_core::Payload::from_json(&serde_json::json!({ "label": "input" }))?,
        run_id: run_id.clone(),
        parent_run_id: None,
    };
    runtime.block_on(store.append(WriteToken::recorder(), &workflow_id, &[started], 0))?;
    let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(store) as _, 1);
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
    Ok(workflow_id)
}

fn install(runtime: &tokio::runtime::Runtime) -> Result<TestContext, Box<dyn std::error::Error>> {
    install_with_timeout(runtime, Some(TEST_QUERY_TIMEOUT))
}

fn install_with_timeout(
    runtime: &tokio::runtime::Runtime,
    query_timeout: Option<Duration>,
) -> Result<TestContext, Box<dyn std::error::Error>> {
    let registry = Arc::new(Registry::default());
    let store = Arc::new(InMemoryStore::default());
    let workflow_id = seed_started_workflow(runtime, &registry, &store, 42)?;
    let engine = Arc::new(FakeEngine::default());
    let state = Arc::new(EngineNifState::default());
    install_query_bridge_with_engine(
        &state,
        TestQueryBridgeParts {
            registry: Arc::clone(&registry),
            engine: engine.clone(),
            runtime: std::sync::Weak::new(),
            tokio_handle: runtime.handle().clone(),
            query_timeout,
            birth_wait: crate::runtime::SignalDeliveryConfig::default(),
        },
    );
    Ok((state, store, engine, workflow_id))
}

#[test]
fn register_query_stores_names_only_and_is_idempotent() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, _store, _engine, _workflow_id) = install(&runtime)?;

    assert_eq!(
        register_query_impl(&state, "state", "{}", Some(42))?,
        "registered"
    );
    assert!(is_query_registered(&state, 42, "state")?);
    assert!(!is_query_registered(&state, 42, "other")?);
    assert!(!is_query_registered(&state, 43, "state")?);
    // Re-registration (replay re-executes registration) stays registered.
    assert_eq!(
        register_query_impl(&state, "state", "{}", Some(42))?,
        "registered"
    );
    assert!(is_query_registered(&state, 42, "state")?);
    Ok(())
}

/// F8 registration race: the start path inserts the registry handle only
/// after the workflow process is spawned, so a workflow whose first
/// instructions register a handler (or service a query) can execute these
/// NIFs before its registry entry exists. The calling pid is authoritative;
/// requiring a registry entry made the birth window a typed failure the SDK
/// treats as fatal — before the fix this test failed with
/// `unknown_workflow_process:77`, and live workflows intermittently died at
/// startup with `{badmatch, {error, <<"unknown_workflow_process:N">>}}`.
#[test]
fn query_nifs_accept_a_pid_the_registry_has_not_inserted_yet() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    // Pid 77 is deliberately never seeded into the registry.
    let (state, _store, _engine, _workflow_id) = install(&runtime)?;

    assert_eq!(
        register_query_impl(&state, "state", "{}", Some(77))?,
        "registered"
    );
    assert!(is_query_registered(&state, 77, "state")?);

    let (sender, receiver) = tokio::sync::oneshot::channel();
    insert_pending_reply(&state, "q-birth".to_owned(), 77, sender)?;
    assert_eq!(
        reply_query_impl(&state, "q-birth", "{\"answer\":1}", Some(77))?,
        "replied"
    );
    let reply = runtime
        .block_on(receiver)?
        .map_err(|error| format!("expected payload, got {error:?}"))?;
    assert_eq!(reply.bytes(), b"{\"answer\":1}");

    let (sender, receiver) = tokio::sync::oneshot::channel();
    insert_pending_reply(&state, "q-birth-err".to_owned(), 77, sender)?;
    assert_eq!(
        reply_query_error_impl(&state, "q-birth-err", "boom", Some(77))?,
        "replied"
    );
    match runtime.block_on(receiver)? {
        Err(QueryError::HandlerFailed { message }) => assert_eq!(message, "boom"),
        other => return Err(format!("expected HandlerFailed, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn reply_query_delivers_pending_response_and_errors_for_missing_id() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, _store, _engine, _workflow_id) = install(&runtime)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    insert_pending_reply(&state, "q-1".to_owned(), 42, sender)?;

    assert_eq!(
        reply_query_impl(&state, "q-1", "{\"answer\":1}", Some(42))?,
        "replied"
    );
    let reply = runtime.block_on(receiver)??;
    assert_eq!(reply.bytes(), b"{\"answer\":1}");
    assert!(reply_query_impl(&state, "missing", "{}", Some(42)).is_err());
    Ok(())
}

#[test]
fn reply_query_error_delivers_typed_handler_failed() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, _store, _engine, _workflow_id) = install(&runtime)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    insert_pending_reply(&state, "q-err".to_owned(), 42, sender)?;

    assert_eq!(
        reply_query_error_impl(&state, "q-err", "handler raised", Some(42))?,
        "replied"
    );

    let reply = runtime.block_on(receiver)?;
    assert_eq!(
        reply,
        Err(QueryError::HandlerFailed {
            message: "handler raised".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn replies_clear_the_servicing_guard_even_for_unknown_ids() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, _store, _engine, _workflow_id) = install(&runtime)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    insert_pending_reply(&state, "q-2".to_owned(), 42, sender)?;
    state.servicing_queries.insert(42, "q-2".to_owned());

    reply_query_impl(&state, "q-2", "{}", Some(42))?;
    assert!(!state.servicing_queries.contains_key(&42));
    drop(runtime.block_on(receiver)?);

    // A late reply for an already-cleaned-up query still lifts the guard so
    // the workflow does not refuse recording NIFs forever.
    state.servicing_queries.insert(42, "stale".to_owned());
    assert!(reply_query_error_impl(&state, "stale", "late", Some(42)).is_err());
    assert!(!state.servicing_queries.contains_key(&42));
    Ok(())
}

#[test]
fn dispatch_query_round_trips_through_query_service() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, store, engine, workflow_id) = install(&runtime)?;
    let target = WorkflowId::new_v4();
    let mut handlers = HashMap::new();
    handlers.insert(
        "state".to_owned(),
        Ok(payload_from_string("{\"visible\":true}")),
    );
    engine.set_workflow(target.clone(), 90, handlers)?;
    let config = serde_json::json!({
        "target_workflow_id": target,
        "payload": "{\"ask\":true}"
    })
    .to_string();

    let reply = dispatch_query_impl(&state, "state", &config, Some(42))?;

    assert_eq!(reply, "{\"visible\":true}");
    let history = runtime.block_on(store.read_history(&workflow_id))?;
    // Only the seeded WorkflowStarted: the query round-trip records nothing.
    assert_eq!(history.len(), 1);
    Ok(())
}

#[test]
fn dispatch_query_without_configured_timeout_fails_typed() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, _store, engine, _workflow_id) = install_with_timeout(&runtime, None)?;
    let target = WorkflowId::new_v4();
    engine.set_workflow(target.clone(), 91, HashMap::new())?;
    let config = serde_json::json!({ "target_workflow_id": target }).to_string();

    let error = dispatch_query_impl(&state, "state", &config, Some(42))
        .err()
        .ok_or("dispatch without a configured timeout unexpectedly succeeded")?;

    assert!(
        error.starts_with("query_timeout_not_configured"),
        "unexpected error: {error}"
    );
    Ok(())
}

#[test]
fn dispatch_query_is_refused_while_servicing_a_query() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, _store, _engine, _workflow_id) = install(&runtime)?;
    state.servicing_queries.insert(42, "q-active".to_owned());
    let config = serde_json::json!({ "target_workflow_id": WorkflowId::new_v4() }).to_string();

    let error = dispatch_query_impl(&state, "state", &config, Some(42))
        .err()
        .ok_or("dispatch during query servicing unexpectedly succeeded")?;

    assert!(
        error.starts_with("query_servicing:dispatch_query"),
        "unexpected error: {error}"
    );
    Ok(())
}

#[test]
fn dispatch_query_is_refused_during_replay() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, store, _engine, workflow_id) = install(&runtime)?;
    // Recorded command history the live execution has not re-issued yet
    // (handle ordinal counters are at zero): the process is mid-replay.
    let scheduled = Event::ActivityScheduled {
        envelope: aion_core::EventEnvelope {
            seq: 2,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        activity_id: aion_core::ActivityId::from_sequence_position(0),
        activity_type: "billing/charge".to_owned(),
        input: aion_core::Payload::from_json(&serde_json::json!({}))?,
    };
    runtime.block_on(store.append(WriteToken::recorder(), &workflow_id, &[scheduled], 1))?;
    let config = serde_json::json!({ "target_workflow_id": WorkflowId::new_v4() }).to_string();

    let error = dispatch_query_impl(&state, "state", &config, Some(42))
        .err()
        .ok_or("dispatch during replay unexpectedly succeeded")?;

    assert!(
        error.starts_with("replay_nondeterministic"),
        "unexpected error: {error}"
    );
    Ok(())
}

#[test]
fn cleanup_process_drains_queries_and_drops_reply_senders() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, _store, _engine, _workflow_id) = install(&runtime)?;
    register_query_impl(&state, "state", "{}", Some(42))?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    insert_pending_reply(&state, "q-3".to_owned(), 42, sender)?;
    state
        .pending_queries
        .entry(42)
        .or_default()
        .push_back(PendingQuery {
            query_id: "q-3".to_owned(),
            name: "state".to_owned(),
        });
    state.servicing_queries.insert(42, "q-3".to_owned());

    state.cleanup_process(42);

    assert!(!state.pending_queries.contains_key(&42));
    assert!(!state.servicing_queries.contains_key(&42));
    assert!(!is_query_registered(&state, 42, "state")?);
    // The dropped sender is the query-racing-completion signal: the waiting
    // caller's oneshot resolves with a channel-closed error, which the
    // QueryService maps to ReplyDropped.
    assert!(runtime.block_on(receiver).is_err());
    Ok(())
}

#[test]
fn query_nifs_do_not_change_event_history() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (state, store, engine, workflow_id) = install(&runtime)?;
    let target = WorkflowId::new_v4();
    let mut handlers = HashMap::new();
    handlers.insert("state".to_owned(), Ok(payload_from_string("{}")));
    engine.set_workflow(target.clone(), 91, handlers)?;
    let before = runtime.block_on(store.read_history(&workflow_id))?;

    register_query_impl(&state, "local", "{}", Some(42))?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    insert_pending_reply(&state, "q-2".to_owned(), 42, sender)?;
    reply_query_impl(&state, "q-2", "{}", Some(42))?;
    let reply = runtime.block_on(receiver)??;
    assert_eq!(reply.bytes(), b"{}");
    let config = serde_json::json!({ "target_workflow_id": target, "payload": "{}" }).to_string();
    dispatch_query_impl(&state, "state", &config, Some(42))?;

    let after = runtime.block_on(store.read_history(&workflow_id))?;
    assert_eq!(before, after);
    // Only the seeded WorkflowStarted: query NIFs record nothing.
    assert_eq!(after.len(), 1);
    Ok(())
}

#[test]
fn delivery_queues_pending_query_and_wakes_the_workflow_process() -> TestResult {
    let tokio_runtime = tokio::runtime::Runtime::new()?;
    let runtime = Arc::new(crate::runtime::RuntimeHandle::new(RuntimeConfig::new(
        Some(1),
    ))?);
    runtime.register_module(
        "aion_fixture_workflow",
        include_bytes!("../../tests/fixtures/aion_fixture_workflow.beam"),
    )?;
    let pid = runtime.spawn_workflow("aion_fixture_workflow", "wait", RuntimeInput::default())?;
    let registry = Arc::new(Registry::default());
    let store = Arc::new(InMemoryStore::default());
    seed_started_workflow(&tokio_runtime, &registry, &store, pid)?;
    let state = Arc::clone(runtime.nif_state());
    let mailbox = install_query_bridge(
        &state,
        Arc::clone(&registry),
        &runtime,
        tokio_runtime.handle().clone(),
        Some(TEST_QUERY_TIMEOUT),
    );
    register_query_impl(&state, "state", "{}", Some(pid))?;

    // Registered name: delivery parks the reply, queues the query, and
    // enqueues the wake marker; nothing replies yet.
    let (reply_to, reply_from) = tokio::sync::oneshot::channel();
    mailbox.deliver_workflow_message(
        WorkflowProcessHandle::new(pid),
        WorkflowMailboxMessage::Query {
            name: "state".to_owned(),
            payload: payload_from_string("{}"),
            reply_to,
        },
    )?;
    let queued = state
        .pending_queries
        .get(&pid)
        .map(|queue| queue.iter().cloned().collect::<Vec<_>>())
        .ok_or("no pending query was queued for the workflow pid")?;
    let [pending] = queued.as_slice() else {
        return Err(format!("expected exactly one queued query, found {}", queued.len()).into());
    };
    assert_eq!(pending.name, "state");
    assert!(pending_reply_is_live(&state, &pending.query_id)?);

    // Unknown name: typed reply to the caller, workflow untouched, queue
    // length unchanged.
    let (unknown_reply_to, unknown_reply_from) = tokio::sync::oneshot::channel();
    mailbox.deliver_workflow_message(
        WorkflowProcessHandle::new(pid),
        WorkflowMailboxMessage::Query {
            name: "missing".to_owned(),
            payload: payload_from_string("{}"),
            reply_to: unknown_reply_to,
        },
    )?;
    assert_eq!(
        tokio_runtime.block_on(unknown_reply_from)?,
        Err(QueryError::UnknownQuery("missing".to_owned()))
    );
    assert_eq!(
        state.pending_queries.get(&pid).map(|queue| queue.len()),
        Some(1)
    );

    // The workflow-side reply resolves the parked caller.
    reply_query_impl(&state, &pending.query_id, "{\"n\":7}", Some(pid))?;
    let reply = tokio_runtime.block_on(reply_from)??;
    assert_eq!(reply.bytes(), b"{\"n\":7}");

    runtime.shutdown()?;
    Ok(())
}
