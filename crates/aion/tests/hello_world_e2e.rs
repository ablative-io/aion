//! End-to-end regression test: the packaged hello-world example must run to
//! completion through the real engine — package load, content-hash module
//! namespacing, entry dispatch, codec decode, live activity dispatch and
//! completion delivery, and durable history recording.
//!
//! This is the test that was missing when the DX-016 entry-contract
//! regression and the constant-pool rename bug shipped.
//!
//! The archive is rebuilt from the committed example source on every run —
//! see `common/example_build.rs` for why this gate must never skip.

#[path = "common/example_build.rs"]
mod example_build;

use std::sync::Arc;

use aion::EngineBuilder;
use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion_core::{Payload, WorkflowStatus};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

struct GreetDispatcher;

impl ActivityDispatcher for GreetDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let name = request.name.as_str();
        let input = request.input.as_str();
        if name != "greet" {
            return Err(format!("terminal:unknown activity {name}"));
        }
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|e| format!("terminal:bad input: {e}"))?;
        let who = value["name"].as_str().unwrap_or("stranger");
        Ok(json!({ "greeting": format!("Hello, {who}! Welcome to Aion.") }).to_string())
    }
}

#[tokio::test]
async fn hello_world_runs_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    let package = example_build::built_package("examples/hello-world", "hello_world")?;

    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .load_workflows(package)
        .build()
        .await?;

    let input = Payload::from_json(&json!({ "name": "Ada" }))?;
    let handle = engine
        .start_workflow(
            "hello_world",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;

    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    let greeting: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(greeting, json!("Hello, Ada! Welcome to Aion."));

    let history = store.read_history(handle.workflow_id()).await?;
    let kinds: Vec<bool> = vec![
        matches!(
            history.first(),
            Some(aion_core::Event::WorkflowStarted { .. })
        ),
        matches!(
            history.get(1),
            Some(aion_core::Event::ActivityScheduled { .. })
        ),
        matches!(
            history.get(2),
            Some(aion_core::Event::ActivityStarted { .. })
        ),
        matches!(
            history.get(3),
            Some(aion_core::Event::ActivityCompleted { .. })
        ),
        matches!(
            history.get(4),
            Some(aion_core::Event::WorkflowCompleted { .. })
        ),
    ];
    assert!(
        history.len() == 5 && kinds.iter().all(|matched| *matched),
        "unexpected history: {history:?}"
    );
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Completed
    );

    engine.shutdown()?;
    Ok(())
}
