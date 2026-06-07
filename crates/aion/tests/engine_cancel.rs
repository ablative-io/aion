//! End-to-end engine cancellation integration tests over `InMemoryStore`.

mod common;

use std::thread;
use std::time::{Duration, Instant};

use aion::dispatch_activity;
use aion_core::Event;
use serde_json::json;

use common::{FIXTURE_MODULE, input_payload, payload};

fn wait_until_not_live(runtime: &aion::RuntimeHandle, pid: aion::Pid) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if !runtime.is_live(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    !runtime.is_live(pid)
}

#[tokio::test]
async fn cancel_records_event_deregisters_and_kills_workflow()
-> Result<(), Box<dyn std::error::Error>> {
    let (engine, store) = common::engine_with_fixture("wait").await?;
    let handle = engine
        .start_workflow(FIXTURE_MODULE, input_payload()?)
        .await?;

    let reason = "caller requested cancellation";
    engine
        .cancel(handle.workflow_id(), handle.run_id(), reason)
        .await?;

    assert!(wait_until_not_live(engine.runtime(), handle.pid()));
    assert!(
        engine
            .registry()
            .get(handle.workflow_id(), handle.run_id())?
            .is_none()
    );

    let history = store.read_history(handle.workflow_id()).await?;
    match history.last() {
        Some(Event::WorkflowCancelled {
            reason: recorded_reason,
            ..
        }) => assert_eq!(recorded_reason, reason),
        other => {
            return Err(format!("expected final WorkflowCancelled event, found {other:?}").into());
        }
    }

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn cancel_propagates_kill_to_linked_activity() -> Result<(), Box<dyn std::error::Error>> {
    let (engine, _store) = common::engine_with_fixture("wait").await?;
    let handle = engine
        .start_workflow(FIXTURE_MODULE, input_payload()?)
        .await?;
    let deployed_module = engine
        .loaded_workflows()
        .latest(FIXTURE_MODULE)
        .ok_or("fixture workflow was not loaded")?
        .deployed_entry_module()
        .to_owned();
    let activity = dispatch_activity(
        engine.runtime(),
        handle.pid(),
        &deployed_module,
        "activity",
        &payload(&json!(null))?,
    )?;

    assert!(engine.runtime().is_live(activity));

    let reason = "caller requested cancellation";
    engine
        .cancel(handle.workflow_id(), handle.run_id(), reason)
        .await?;

    assert!(wait_until_not_live(engine.runtime(), handle.pid()));
    assert!(wait_until_not_live(engine.runtime(), activity));

    engine.shutdown()?;
    Ok(())
}
