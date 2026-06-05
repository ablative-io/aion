//! End-to-end engine start/list integration tests over `InMemoryStore`.

mod common;

use aion::{HandleResidency, RuntimeConfig, RuntimeHandle};
use aion_core::{Event, WorkflowFilter, WorkflowStatus};
use serde_json::json;

use common::{FIXTURE_MODULE, fixture_package, input_payload, payload};

#[test]
fn fixture_beam_registers_and_entry_function_resolves() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
    let package = fixture_package("complete")?;
    let deployed = package.deployed_entry_module();
    let beam = package
        .deployed_modules()
        .into_iter()
        .find_map(|(name, bytes)| (name == deployed).then_some(bytes.to_vec()))
        .ok_or("fixture package did not expose its deployed entry module")?;

    runtime.register_module(&deployed, &beam)?;

    assert!(runtime.has_registered_module(&deployed));
    let pid = runtime.spawn_workflow(&deployed, "complete", aion::RuntimeInput::default())?;
    assert!(runtime.is_live(pid));
    assert_eq!(runtime.workflow_outcome(pid)?, Ok(payload(&json!(42))?));
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn start_appends_registers_and_lists_workflow() -> Result<(), Box<dyn std::error::Error>> {
    let (engine, store) = common::engine_with_fixture("wait").await?;
    let input = input_payload()?;

    let handle = engine.start_workflow(FIXTURE_MODULE, input.clone()).await?;

    let history = store.read_history(handle.workflow_id()).await?;
    match history.first() {
        Some(Event::WorkflowStarted {
            workflow_type,
            input: recorded_input,
            ..
        }) => {
            assert_eq!(workflow_type, FIXTURE_MODULE);
            assert_eq!(recorded_input, &input);
        }
        other => {
            return Err(format!("expected first WorkflowStarted event, found {other:?}").into());
        }
    }

    let registered = engine
        .registry()
        .get(handle.workflow_id(), handle.run_id())?
        .ok_or("started workflow was not registered")?;
    assert_eq!(registered.cached_status(), WorkflowStatus::Running);
    assert_eq!(registered.residency(), HandleResidency::Resident);

    let summaries = engine.list_workflows(WorkflowFilter::default()).await?;
    let summary = summaries
        .iter()
        .find(|summary| summary.workflow_id == *handle.workflow_id())
        .ok_or("started workflow was absent from list_workflows")?;
    assert_eq!(summary.workflow_type, FIXTURE_MODULE);
    assert_eq!(summary.status, WorkflowStatus::Running);
    assert_eq!(input.to_json()?, json!({ "fixture": "input" }));

    engine.shutdown()?;
    Ok(())
}
