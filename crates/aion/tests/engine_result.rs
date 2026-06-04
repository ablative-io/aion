//! End-to-end engine completion/result integration tests over `InMemoryStore`.

mod common;

use aion_core::{Event, WorkflowFilter, WorkflowStatus};
use serde_json::json;

use common::{FIXTURE_MODULE, input_payload, payload};

#[tokio::test]
async fn completing_workflow_records_and_returns_result() -> Result<(), Box<dyn std::error::Error>>
{
    let (engine, store) = common::engine_with_fixture("complete").await?;
    let handle = engine
        .start_workflow(FIXTURE_MODULE, input_payload()?)
        .await?;
    let expected = payload(json!(42))?;

    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;

    assert_eq!(result, Ok(expected.clone()));
    let history = store.read_history(handle.workflow_id()).await?;
    match history.last() {
        Some(Event::WorkflowCompleted { result, .. }) => assert_eq!(result, &expected),
        other => {
            return Err(format!("expected final WorkflowCompleted event, found {other:?}").into());
        }
    }

    let summaries = engine.list_workflows(WorkflowFilter::default()).await?;
    let summary = summaries
        .iter()
        .find(|summary| summary.workflow_id == *handle.workflow_id())
        .ok_or("completed workflow was absent from list_workflows")?;
    assert_eq!(summary.workflow_type, FIXTURE_MODULE);
    assert_eq!(summary.status, WorkflowStatus::Completed);

    engine.shutdown()?;
    Ok(())
}
