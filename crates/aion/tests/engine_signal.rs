//! Engine signal integration tests over the concrete delegated signal router.

mod common;

use std::sync::Arc;

use aion::durability::Recorder;
use aion::{
    EngineBuilder, EngineError, HandleResidency, RuntimeHandle, SignalRouter,
    signal::ConcreteSignalRouter,
};
use aion_core::{Event, RunId, WorkflowId};
use aion_store::{EventStore, InMemoryStore};
use chrono::Utc;
use serde_json::json;

use common::{FIXTURE_MODULE, engine_with_fixture, fixture_package, input_payload, payload};

#[tokio::test]
async fn signal_records_history_and_delivers_mailbox_marker()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .load_workflows(fixture_package("wait")?)
        .build()
        .await?;
    let input = input_payload()?;
    let handle = engine.start_workflow(FIXTURE_MODULE, input).await?;
    let sent_payload = payload(&json!({ "wake": true }))?;

    engine
        .signal(
            handle.workflow_id(),
            handle.run_id(),
            "wake",
            sent_payload.clone(),
        )
        .await?;

    let history = store.read_history(handle.workflow_id()).await?;
    let signal = history
        .iter()
        .find_map(|event| match event {
            Event::SignalReceived {
                name,
                payload,
                envelope,
            } => Some((name, payload, envelope)),
            _ => None,
        })
        .ok_or("SignalReceived event was not recorded")?;
    assert_eq!(signal.0, "wake");
    assert_eq!(signal.1, &sent_payload);
    assert_eq!(signal.2.workflow_id, *handle.workflow_id());
    assert_eq!(signal.2.seq, 2);
    // The wake marker carries no payload: awaits resolve the durable
    // SignalReceived above. signal() returning Ok proves the marker was
    // enqueued to the resident process after the record succeeded.

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn signal_to_killed_run_returns_terminal_without_appending()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .load_workflows(fixture_package("wait")?)
        .build()
        .await?;
    let handle = engine
        .start_workflow(FIXTURE_MODULE, input_payload()?)
        .await?;
    engine.runtime().cancel_pid(handle.pid())?;
    // Await the exit monitor's durable terminal record so the signal below
    // deterministically targets a terminal run.
    let outcome = engine.result(handle.workflow_id(), handle.run_id()).await?;
    assert!(
        outcome.is_err(),
        "killed run should report a failed outcome"
    );
    let terminal_history_len = store.read_history(handle.workflow_id()).await?.len();

    let error = engine
        .signal(
            handle.workflow_id(),
            handle.run_id(),
            "wake",
            payload(&json!({ "wake": "rejected" }))?,
        )
        .await
        .err()
        .ok_or("signal to terminal run unexpectedly succeeded")?;

    assert!(matches!(
        error,
        EngineError::SignalRouter(aion::SignalRouterError::Terminal { .. })
    ));
    assert_eq!(
        store.read_history(handle.workflow_id()).await?.len(),
        terminal_history_len
    );

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn terminal_and_unknown_signals_return_errors_without_appending_events()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .load_workflows(fixture_package("wait")?)
        .build()
        .await?;
    let handle = engine
        .start_workflow(FIXTURE_MODULE, input_payload()?)
        .await?;
    let mut recorder = Recorder::resume_at(handle.workflow_id().clone(), Arc::clone(&store), 1);
    recorder
        .record_workflow_completed(Utc::now(), payload(&json!({ "done": true }))?)
        .await?;
    engine
        .registry()
        .remove(handle.workflow_id(), handle.run_id())?;
    let terminal_history_len = store.read_history(handle.workflow_id()).await?.len();

    let terminal_error = engine
        .signal(
            handle.workflow_id(),
            handle.run_id(),
            "ignored",
            payload(&json!(null))?,
        )
        .await
        .err()
        .ok_or("terminal workflow signal unexpectedly succeeded")?;
    assert!(matches!(
        terminal_error,
        EngineError::SignalRouter(aion::SignalRouterError::Terminal { .. })
    ));
    assert_eq!(
        store.read_history(handle.workflow_id()).await?.len(),
        terminal_history_len
    );

    let unknown_workflow_id = WorkflowId::new_v4();
    let unknown_run_id = RunId::new_v4();
    let unknown_error = engine
        .signal(
            &unknown_workflow_id,
            &unknown_run_id,
            "ignored",
            payload(&json!(null))?,
        )
        .await
        .err()
        .ok_or("unknown workflow signal unexpectedly succeeded")?;
    assert!(matches!(
        unknown_error,
        EngineError::WorkflowNotFound { .. }
    ));
    assert!(store.read_history(&unknown_workflow_id).await?.is_empty());

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn non_resident_signal_records_defers_and_resume_delivers()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .load_workflows(fixture_package("wait")?)
        .build()
        .await?;
    let handle = engine
        .start_workflow(FIXTURE_MODULE, input_payload()?)
        .await?;
    engine.registry().replace_residency(
        handle.workflow_id(),
        handle.run_id(),
        HandleResidency::Suspended,
    )?;
    let sent_payload = payload(&json!({ "wake": "later" }))?;

    engine
        .signal(
            handle.workflow_id(),
            handle.run_id(),
            "wake",
            sent_payload.clone(),
        )
        .await?;

    let history = store.read_history(handle.workflow_id()).await?;
    let signal = history
        .iter()
        .find_map(|event| match event {
            Event::SignalReceived { name, payload, .. } => Some((name, payload)),
            _ => None,
        })
        .ok_or("SignalReceived event was not recorded for suspended workflow")?;
    assert_eq!(signal.0, "wake");
    assert_eq!(signal.1, &sent_payload);
    assert_eq!(
        engine
            .signal_handoff()
            .pending_count(handle.workflow_id())?,
        1
    );

    let resumed = engine.resume_workflow(handle.workflow_id(), handle.run_id())?;
    assert_eq!(resumed.residency(), HandleResidency::Resident);
    // The deferred queue drains by delivering wake markers to the resumed
    // process; the payload itself stays in the durable history above.
    assert_eq!(
        engine
            .signal_handoff()
            .pending_count(handle.workflow_id())?,
        0
    );

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn deferred_signal_router_returns_runtime_error() -> Result<(), Box<dyn std::error::Error>> {
    let (engine, _store) = engine_with_fixture("wait").await?;
    let handle = engine
        .start_workflow(FIXTURE_MODULE, input_payload()?)
        .await?;
    let sent_payload = payload(&json!({ "ignored": true }))?;

    let error = engine
        .signal(handle.workflow_id(), handle.run_id(), "test", sent_payload)
        .await
        .err()
        .ok_or("deferred signal router should return an error")?;
    assert!(matches!(error, EngineError::Runtime { .. }));

    engine.shutdown()?;
    Ok(())
}
