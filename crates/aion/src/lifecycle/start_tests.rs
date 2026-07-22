//! Tests for workflow start lifecycle.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use aion_core::{Event, Payload, WorkflowId};
use aion_package::ContentHash;
use aion_store::visibility::VisibilityStore;
use aion_store::{EventStore, InMemoryStore, ReadableEventStore};
use serde_json::json;

use super::{
    StartWorkflowContext, StartWorkflowOptions, start_workflow, start_workflow_with_options,
};
use crate::EngineError;
use crate::loader::WorkflowCatalog;
use crate::registry::{HandleResidency, Registry};
use crate::runtime::{RuntimeConfig, RuntimeHandle};
use crate::supervision::SupervisionTree;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "label": label }))
}

fn load_without_runtime_registration(workflow_type: &str) -> Arc<WorkflowCatalog> {
    let catalog = Arc::new(WorkflowCatalog::new());
    catalog.note_loaded_workflow_for_test(
        workflow_type,
        format!("{workflow_type}__deployed"),
        "run",
        ContentHash::from_bytes([3; 32]),
    );
    catalog
}

fn context(
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    catalog: Arc<WorkflowCatalog>,
    runtime: Arc<RuntimeHandle>,
    supervision: Arc<SupervisionTree>,
    registry: Arc<Registry>,
) -> StartWorkflowContext {
    StartWorkflowContext {
        store,
        visibility_store,
        catalog,
        runtime,
        supervision,
        registry,
        signal_handoff: None,
        search_attribute_schema: Arc::new(aion_core::SearchAttributeSchema::new()),
        monitor_tokio_handle: tokio::runtime::Handle::current(),
    }
}

#[tokio::test]
async fn unknown_workflow_type_returns_not_found_and_appends_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(InMemoryStore::default());
    let catalog = Arc::new(WorkflowCatalog::new());
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let supervision = Arc::new(SupervisionTree::new());
    let registry = Arc::new(Registry::default());
    let input = payload("input")?;

    let result = start_workflow(
        context(
            store.clone(),
            store.clone(),
            Arc::clone(&catalog),
            Arc::clone(&runtime),
            Arc::clone(&supervision),
            Arc::clone(&registry),
        ),
        "checkout",
        input,
    )
    .await;

    assert!(matches!(
        result,
        Err(EngineError::WorkflowNotFound { workflow_type }) if workflow_type == "checkout"
    ));
    assert_eq!(store.list_active().await?, Vec::new());
    assert_eq!(registry.list()?.len(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn recorder_append_happens_before_spawn_failure() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(InMemoryStore::default());
    let catalog = load_without_runtime_registration("checkout");
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let supervision = Arc::new(SupervisionTree::new());
    let registry = Arc::new(Registry::default());
    let input = payload("input")?;

    let result = start_workflow(
        context(
            store.clone(),
            store.clone(),
            Arc::clone(&catalog),
            Arc::clone(&runtime),
            Arc::clone(&supervision),
            Arc::clone(&registry),
        ),
        "checkout",
        input.clone(),
    )
    .await;

    assert!(matches!(result, Err(EngineError::Runtime { .. })));
    let active = store.list_active().await?;
    assert_eq!(active.len(), 1);
    let history = store.read_history(&active[0]).await?;
    assert_eq!(history.len(), 1);
    match &history[0] {
        Event::WorkflowStarted {
            envelope,
            workflow_type,
            input: recorded_input,
            run_id: _,
            parent_run_id: None,
            ..
        } => {
            assert_eq!(envelope.seq, 1);
            assert_eq!(&envelope.workflow_id, &active[0]);
            assert_eq!(workflow_type, "checkout");
            assert_eq!(recorded_input, &input);
        }
        other => return Err(format!("expected WorkflowStarted, found {other:?}").into()),
    }
    assert_eq!(registry.list()?.len(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn start_with_attributes_records_started_then_attributes_before_spawn()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(InMemoryStore::default());
    let deployed_module = "checkout__deployed";
    let catalog = load_without_runtime_registration("checkout");
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.register_waiting_test_module(deployed_module, "run");
    let supervision = Arc::new(SupervisionTree::new());
    let registry = Arc::new(Registry::default());
    let mut schema = aion_core::SearchAttributeSchema::new();
    schema.register("aion.namespace", aion_core::SearchAttributeType::String)?;
    let attributes = std::collections::HashMap::from([(
        String::from("aion.namespace"),
        aion_core::SearchAttributeValue::String(String::from("tenant-a")),
    )]);

    let handle = start_workflow_with_options(
        StartWorkflowContext {
            store: store.clone(),
            visibility_store: store.clone(),
            catalog: Arc::clone(&catalog),
            runtime: Arc::clone(&runtime),
            supervision: Arc::clone(&supervision),
            registry: Arc::clone(&registry),
            signal_handoff: None,
            search_attribute_schema: Arc::new(schema),
            monitor_tokio_handle: tokio::runtime::Handle::current(),
        },
        "checkout",
        payload("input")?,
        StartWorkflowOptions {
            search_attributes: attributes.clone(),
            ..StartWorkflowOptions::default()
        },
    )
    .await?;

    let history = store.read_history(handle.workflow_id()).await?;
    match history.as_slice() {
        [
            Event::WorkflowStarted { .. },
            Event::SearchAttributesUpdated {
                attributes: recorded,
                ..
            },
        ] => assert_eq!(recorded, &attributes),
        other => {
            return Err(format!("expected started then attributes, found {other:?}").into());
        }
    }
    let summaries = store
        .list_workflows(aion_store::visibility::ListWorkflowsFilter::default())
        .await?;
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].search_attributes, attributes);
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn start_with_unregistered_attribute_fails_without_append_or_spawn()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(InMemoryStore::default());
    let deployed_module = "checkout__deployed";
    let catalog = load_without_runtime_registration("checkout");
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.register_waiting_test_module(deployed_module, "run");
    let supervision = Arc::new(SupervisionTree::new());
    let registry = Arc::new(Registry::default());

    let result = start_workflow_with_options(
        context(
            store.clone(),
            store.clone(),
            Arc::clone(&catalog),
            Arc::clone(&runtime),
            Arc::clone(&supervision),
            Arc::clone(&registry),
        ),
        "checkout",
        payload("input")?,
        StartWorkflowOptions {
            search_attributes: std::collections::HashMap::from([(
                String::from("aion.namespace"),
                aion_core::SearchAttributeValue::String(String::from("tenant-a")),
            )]),
            ..StartWorkflowOptions::default()
        },
    )
    .await;

    assert!(matches!(result, Err(EngineError::Durability(_))));
    assert_eq!(store.list_active().await?, Vec::new());
    assert_eq!(registry.list()?.len(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn successful_start_appends_spawns_places_registers_and_returns_handle()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(InMemoryStore::default());
    let deployed_module = "checkout__deployed";
    let catalog = load_without_runtime_registration("checkout");
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.register_waiting_test_module(deployed_module, "run");
    let supervision = Arc::new(SupervisionTree::new());
    let registry = Arc::new(Registry::default());
    let input = payload("input")?;

    let handle = start_workflow(
        context(
            store.clone(),
            store.clone(),
            Arc::clone(&catalog),
            Arc::clone(&runtime),
            Arc::clone(&supervision),
            Arc::clone(&registry),
        ),
        "checkout",
        input.clone(),
    )
    .await?;

    assert_eq!(handle.workflow_type(), "checkout");
    assert_eq!(handle.loaded_version(), &ContentHash::from_bytes([3; 32]));
    assert_eq!(handle.cached_status(), aion_core::WorkflowStatus::Running);
    assert_eq!(handle.residency(), HandleResidency::Resident);
    assert!(!handle.completion().is_completed());
    runtime.wait_for_process_ready(handle.pid())?;
    assert!(runtime.trap_exit(handle.pid())?);
    assert_eq!(
        supervision
            .workflow(handle.pid())?
            .map(|node| node.workflow_pid()),
        Some(handle.pid())
    );

    let registered = registry.get(handle.workflow_id(), handle.run_id())?;
    assert_eq!(registered, Some(handle.clone()));
    let active = store.list_active().await?;
    assert_eq!(active, vec![handle.workflow_id().clone()]);
    let history = store.read_history(handle.workflow_id()).await?;
    assert_eq!(history.len(), 1);
    match &history[0] {
        Event::WorkflowStarted {
            envelope,
            workflow_type,
            input: recorded_input,
            run_id: recorded_run_id,
            parent_run_id: None,
            ..
        } => {
            assert_eq!(envelope.seq, 1);
            assert_eq!(&envelope.workflow_id, handle.workflow_id());
            assert_eq!(workflow_type, "checkout");
            assert_eq!(recorded_input, &input);
            assert_eq!(recorded_run_id, handle.run_id());
        }
        other => return Err(format!("expected WorkflowStarted, found {other:?}").into()),
    }
    runtime.shutdown()?;
    Ok(())
}

struct DelayedRegistryDelivery {
    resolved: mpsc::Receiver<u64>,
    release: mpsc::SyncSender<()>,
    result: mpsc::Receiver<Result<(), EngineError>>,
    worker: std::thread::JoinHandle<()>,
}

fn spawn_delayed_registry_delivery(
    registry: Arc<Registry>,
    runtime: Arc<RuntimeHandle>,
    workflow_id: WorkflowId,
) -> Result<DelayedRegistryDelivery, std::io::Error> {
    let (resolved_sender, resolved) = mpsc::sync_channel(1);
    let (release, release_receiver) = mpsc::sync_channel(1);
    let (delivery_sender, result) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name(String::from("aion-start-window-delivery-test"))
        .spawn(move || {
            let delivery = registry
                .live_pid(&workflow_id)
                .and_then(|resolved| {
                    resolved.ok_or_else(|| EngineError::Runtime {
                        reason: String::from("delayed delivery did not resolve the real registry"),
                    })
                })
                .and_then(|resolved| {
                    let _ = resolved_sender.send(resolved);
                    release_receiver
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|_| EngineError::Runtime {
                            reason: String::from("delayed delivery release was not observed"),
                        })?;
                    runtime.deliver_activity_completion_message_with_attempt(
                        resolved,
                        "activity:42",
                        String::from(r#"{"late":true}"#),
                        Some(5),
                    )
                });
            let _ = delivery_sender.send(delivery);
        })?;
    Ok(DelayedRegistryDelivery {
        resolved,
        release,
        result,
        worker,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_start_monitor_failure_retracts_publication_and_observes_abort() -> TestResult {
    let store = Arc::new(InMemoryStore::default());
    let catalog = load_without_runtime_registration("monitor-failure");
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.register_waiting_test_module("monitor-failure__deployed", "run");
    let supervision = Arc::new(SupervisionTree::new());
    let registry = Arc::new(Registry::default());
    let baseline_gates = runtime.activity_delivery_gate_count();
    runtime.pause_next_start_publication_for_test();
    runtime.force_next_monitor_installation_failure_for_test();

    let start = tokio::spawn(start_workflow(
        context(
            store.clone(),
            store,
            Arc::clone(&catalog),
            Arc::clone(&runtime),
            Arc::clone(&supervision),
            Arc::clone(&registry),
        ),
        "monitor-failure",
        payload("input")?,
    ));
    let published = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(handle) = registry.list()?.into_iter().next() {
                return Ok::<_, EngineError>(handle);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| "normal start did not publish before the deadline")??;
    let pid = published.pid();
    runtime.wait_for_start_publication_pause_for_test(pid)?;
    let real_registry_pid = registry
        .live_pid(published.workflow_id())?
        .ok_or("real registry did not resolve the just-published workflow")?;
    assert_eq!(real_registry_pid, pid);

    runtime.deliver_activity_completion_message_with_attempt(
        real_registry_pid,
        "activity:41",
        String::from(r#"{"published":true}"#),
        Some(3),
    )?;
    assert_eq!(runtime.retained_activity_completions(), 1);
    assert_eq!(runtime.retained_activity_attempt_count_for_test(), 1);

    let delayed = spawn_delayed_registry_delivery(
        Arc::clone(&registry),
        Arc::clone(&runtime),
        published.workflow_id().clone(),
    )?;
    assert_eq!(delayed.resolved.recv_timeout(Duration::from_secs(10))?, pid);

    runtime.release_start_publication_for_test(pid)?;
    let error = start
        .await?
        .err()
        .ok_or("forced normal-start monitor failure returned a handle")?;
    assert!(error.to_string().contains("forced test failure"));
    assert!(registry.list()?.is_empty());
    assert!(!runtime.is_live(pid));
    assert!(runtime.process_cleanup_complete_for_test(pid));
    delayed.release.send(())?;
    let delayed_error = delayed
        .result
        .recv_timeout(Duration::from_secs(10))?
        .err()
        .ok_or("delivery resolved before retraction was not refused after abort")?;
    assert!(delayed_error.to_string().contains("not live"));
    delayed
        .worker
        .join()
        .map_err(|_| "delayed registry-resolved delivery thread failed")?;
    assert_eq!(runtime.retained_activity_completions(), 0);
    assert_eq!(runtime.retained_activity_attempt_count_for_test(), 0);
    assert_eq!(runtime.activity_delivery_gate_count(), baseline_gates);
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_start_with_unabortable_process_retains_ownership_not_orphan() -> TestResult {
    // Double failure: the completion monitor install fails (reaching the
    // fail-the-start path) AND the bounded cleanup queue is shut down, so the
    // unmonitored process abort submission is Unavailable. Termination cannot be
    // guaranteed, so ownership MUST be retained rather than retracted — otherwise
    // the still-live process becomes an unowned, unmonitored orphan.
    let store = Arc::new(InMemoryStore::default());
    let catalog = load_without_runtime_registration("retain-on-abort-failure");
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.register_waiting_test_module("retain-on-abort-failure__deployed", "run");
    let supervision = Arc::new(SupervisionTree::new());
    let registry = Arc::new(Registry::default());
    runtime.force_next_monitor_installation_failure_for_test();
    runtime.shutdown_cleanup_executor_for_test()?;

    let result = start_workflow(
        context(
            store.clone(),
            store.clone(),
            Arc::clone(&catalog),
            Arc::clone(&runtime),
            Arc::clone(&supervision),
            Arc::clone(&registry),
        ),
        "retain-on-abort-failure",
        payload("input")?,
    )
    .await;

    let error = result
        .err()
        .ok_or("forced monitor failure returned a handle")?;
    // The start fails with a typed runtime error (the monitor install failure,
    // or the executor-unavailable abort its rollback then hit — both legitimate).
    assert!(
        matches!(error, EngineError::Runtime { .. }),
        "expected a typed runtime failure, got {error:?}"
    );
    // Ownership is retained: the run is NOT retracted while its process cannot be
    // confirmed terminated.
    let retained = registry.list()?;
    assert_eq!(
        retained.len(),
        1,
        "a failed start whose process could not be aborted retains registry ownership"
    );
    let pid = retained[0].pid();
    // The process was never orphaned: it is still live AND still owned, and its
    // cleanup never started (abort submission was refused).
    assert!(
        runtime.is_live(pid),
        "the un-abortable process stays live under retained ownership"
    );
    assert!(!runtime.process_cleanup_started_for_test(pid));
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn start_with_existing_workflow_id_resumes_history_sequence()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(InMemoryStore::default());
    let deployed_module = "checkout__deployed";
    let catalog = load_without_runtime_registration("checkout");
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.register_waiting_test_module(deployed_module, "run");
    let supervision = Arc::new(SupervisionTree::new());
    let registry = Arc::new(Registry::default());
    let workflow_id = aion_core::WorkflowId::new_v4();
    let parent_run_id = aion_core::RunId::new_v4();

    let mut recorder = crate::durability::Recorder::new(workflow_id.clone(), store.clone());
    recorder
        .record_workflow_started(
            chrono::Utc::now(),
            crate::durability::WorkflowStartRecord {
                workflow_type: "checkout".to_owned(),
                input: payload("first")?,
                run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
        )
        .await?;
    recorder
        .record_workflow_continued_as_new(
            chrono::Utc::now(),
            payload("second")?,
            None,
            parent_run_id.clone(),
        )
        .await?;

    let handle = start_workflow_with_options(
        context(
            store.clone(),
            store.clone(),
            Arc::clone(&catalog),
            Arc::clone(&runtime),
            Arc::clone(&supervision),
            Arc::clone(&registry),
        ),
        "checkout",
        payload("second")?,
        StartWorkflowOptions {
            workflow_id: Some(workflow_id.clone()),
            routing_key: None,
            parent_run_id: Some(parent_run_id.clone()),
            loaded_version: None,
            search_attributes: std::collections::HashMap::new(),
            namespace: None,
        },
    )
    .await?;

    assert_eq!(handle.workflow_id(), &workflow_id);
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(history.len(), 3);
    match &history[2] {
        Event::WorkflowStarted {
            envelope,
            workflow_type,
            run_id: _,
            parent_run_id: started_parent,
            ..
        } => {
            assert_eq!(envelope.seq, 3);
            assert_eq!(workflow_type, "checkout");
            assert_eq!(started_parent, &Some(parent_run_id));
        }
        other => {
            return Err(format!("expected replacement WorkflowStarted, found {other:?}").into());
        }
    }
    runtime.shutdown()?;
    Ok(())
}
