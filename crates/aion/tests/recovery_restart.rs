//! Engine restart recovery integration tests.

mod common;

use std::sync::Arc;

use aion::EngineBuilder;
use aion_core::{Event, WorkflowStatus, status_from_events};
use aion_store::{EventStore, InMemoryStore};

use common::{FIXTURE_MODULE, fixture_package, input_payload};

#[tokio::test]
async fn restart_recovers_active_workflow_without_duplicate_replay_events()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let package = fixture_package("wait")?;

    let first = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package.clone())
        .build()
        .await?;
    let handle = first
        .start_workflow(FIXTURE_MODULE, input_payload()?)
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();
    let pre_restart_history = store.read_history(&workflow_id).await?;
    assert_eq!(pre_restart_history.len(), 1);
    assert!(matches!(
        pre_restart_history[0],
        Event::WorkflowStarted { .. }
    ));
    first.shutdown()?;

    let recovered = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?;
    let recovered_handle = recovered
        .registry()
        .get(&workflow_id, &run_id)?
        .ok_or("recovered workflow was not registered")?;
    assert_eq!(recovered_handle.workflow_type(), FIXTURE_MODULE);
    assert!(recovered.runtime().is_live(recovered_handle.pid()));

    let post_recovery_history = store.read_history(&workflow_id).await?;
    assert_eq!(post_recovery_history, pre_restart_history);
    assert_eq!(
        post_recovery_history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
            .count(),
        1
    );

    recovered
        .cancel(&workflow_id, &run_id, "integration test completion")
        .await?;
    let terminal_history = store.read_history(&workflow_id).await?;
    assert_eq!(terminal_history.len(), pre_restart_history.len() + 1);
    assert_eq!(
        status_from_events(&terminal_history),
        WorkflowStatus::Cancelled
    );
    assert_eq!(
        terminal_history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
            .count(),
        1
    );

    recovered.shutdown()?;
    Ok(())
}
