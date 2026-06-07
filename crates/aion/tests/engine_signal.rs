//! Engine signal integration tests over the concrete delegated signal router.

mod common;

use std::sync::Arc;

use aion::{EngineBuilder, EngineError, RuntimeHandle, SignalRouter, signal::ConcreteSignalRouter};
use aion_core::{Event, RunId, WorkflowId};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

use common::{FIXTURE_MODULE, engine_with_fixture, fixture_package, input_payload, payload};

#[tokio::test]
async fn signal_records_history_and_delivers_mailbox_marker()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>| {
            Arc::new(ConcreteSignalRouter::new(runtime)) as Arc<dyn SignalRouter>
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

    let delivered = engine.runtime().signal_messages(handle.pid());
    assert_eq!(delivered, vec![("wake".to_owned(), sent_payload)]);

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
    assert_eq!(
        store.read_history(handle.workflow_id()).await?.len(),
        history.len()
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
