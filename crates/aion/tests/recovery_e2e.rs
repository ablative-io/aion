//! End-to-end crash-recovery regression test.
//!
//! A workflow whose durable history ends mid-flight (only `WorkflowStarted`
//! recorded — the engine crashed before any further progress) must be
//! recovered when a new engine builds over the same store: the production
//! AD seam re-spawns the workflow process from the recorded start metadata,
//! replay resumes it, and live execution drives it to completion without
//! duplicating any recorded event.

use std::sync::Arc;

use aion::EngineBuilder;
use aion::activity::bridge::ActivityDispatcher;
use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus};
use aion_package::{ExtractionLimits, Package};
use aion_store::{EventStore, InMemoryStore, WriteToken};
use chrono::Utc;
use serde_json::json;

struct GreetDispatcher;

impl ActivityDispatcher for GreetDispatcher {
    fn dispatch(
        &self,
        name: &str,
        input: &str,
        _config: &str,
        _attempt: u32,
    ) -> Result<String, String> {
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
async fn interrupted_workflow_recovers_and_completes() -> Result<(), Box<dyn std::error::Error>> {
    let archive_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/hello-world/hello-world.aion"
    );
    if !std::path::Path::new(archive_path).exists() {
        eprintln!(
            "skipping interrupted_workflow_recovers_and_completes: {archive_path} not built \
             (run `cargo run -p aion-cli -- package examples/hello-world --build`)"
        );
        return Ok(());
    }
    let package =
        Package::load_from_bytes(std::fs::read(archive_path)?, ExtractionLimits::unbounded())?;

    // Simulate the crash: durable history holds only the start event.
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let input = Payload::from_json(&json!({ "name": "Ada" }))?;
    store
        .append(
            WriteToken::recorder(),
            &workflow_id,
            &[Event::WorkflowStarted {
                envelope: EventEnvelope {
                    seq: 1,
                    recorded_at: Utc::now(),
                    workflow_id: workflow_id.clone(),
                },
                workflow_type: "hello_world".to_owned(),
                input,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new(package.content_hash().to_string()),
            }],
            0,
        )
        .await?;

    // Building the engine performs startup recovery through the production
    // AD seam: the active history is re-spawned as a live process.
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .load_workflows(package)
        .build()
        .await?;

    let recovered = engine.registry().get(&workflow_id, &run_id)?;
    assert!(
        recovered.is_some_and(|handle| handle.workflow_type() == "hello_world"
            && handle.cached_status() == WorkflowStatus::Running),
        "recovered workflow must be registered as a running resident process"
    );
    assert!(
        engine
            .supervision()
            .type_supervisors()?
            .iter()
            .any(|node| node.id().workflow_type() == "hello_world"),
        "recovered workflow type must be supervised"
    );

    // The recovered process replays the recorded start and then runs live to
    // completion.
    let result = engine.result(&workflow_id, &run_id).await?;
    let payload = result.map_err(|error| format!("recovered workflow failed: {error:?}"))?;
    let greeting: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(greeting, json!("Hello, Ada! Welcome to Aion."));

    let history = store.read_history(&workflow_id).await?;
    assert_eq!(
        history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
            .count(),
        1,
        "recovery must not re-record WorkflowStarted: {history:?}"
    );
    assert!(
        matches!(history.last(), Some(Event::WorkflowCompleted { .. })),
        "unexpected history: {history:?}"
    );
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Completed
    );

    engine.shutdown()?;
    Ok(())
}
