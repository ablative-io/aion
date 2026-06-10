//! Example-archive regression tests.
//!
//! Every example package under `examples/` must load into the engine, and
//! the activity-only data-pipeline example must run end-to-end: fan-out
//! fetches, concurrent per-item processing, and a fan-in aggregation — the
//! same path remote workers exercise, served here by an in-process
//! dispatcher. Archives are build artifacts, so each test skips (with a
//! notice) when its archive has not been produced yet.

use std::sync::Arc;

use aion::EngineBuilder;
use aion::activity::bridge::ActivityDispatcher;
use aion_core::Payload;
use aion_package::Package;
use aion_store::InMemoryStore;
use serde_json::json;

const EXAMPLE_ARCHIVES: &[(&str, &str)] = &[
    ("approval-gate", "approval-gate/approval-gate.aion"),
    (
        "batch-orchestrator",
        "batch-orchestrator/batch-orchestrator.aion",
    ),
    ("data-pipeline", "data-pipeline/data-pipeline.aion"),
    ("subscription", "subscription/subscription.aion"),
    (
        "agent-orchestration",
        "agent-orchestration/orchestrator.aion",
    ),
    ("order-saga", "order-saga/order-saga.aion"),
    ("hello-world", "hello-world/hello-world.aion"),
];

fn examples_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

#[tokio::test]
async fn every_example_archive_loads_into_the_engine() -> Result<(), Box<dyn std::error::Error>> {
    let root = examples_root();
    for (name, relative) in EXAMPLE_ARCHIVES {
        let path = root.join(relative);
        if !path.exists() {
            eprintln!("skipping {name}: {} not built", path.display());
            continue;
        }
        let package = Package::load_from_bytes(std::fs::read(&path)?)?;
        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .scheduler_threads(1)
            .load_workflows(package)
            .build()
            .await
            .map_err(|error| format!("{name} failed to load: {error}"))?;
        assert!(
            engine.loaded_workflows().iter().count() > 0,
            "{name} loaded no workflow versions"
        );
        engine.shutdown()?;
    }
    Ok(())
}

struct PipelineDispatcher;

impl ActivityDispatcher for PipelineDispatcher {
    fn dispatch(&self, name: &str, input: &str, _config: &str) -> Result<String, String> {
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|e| format!("terminal:bad input: {e}"))?;
        match name {
            // The example encodes the fetch input as a bare JSON string.
            "fetch_url" => {
                let url = value.as_str().unwrap_or_default();
                Ok(json!({
                    "url": url,
                    "content": format!("contents of {url} with five words")
                })
                .to_string())
            }
            "process_item" => {
                let url = value["url"].as_str().unwrap_or_default();
                let content = value["content"].as_str().unwrap_or_default();
                let words = content.split_whitespace().count();
                Ok(json!({
                    "url": url,
                    "word_count": words,
                    "summary": format!("{url}: {words} words")
                })
                .to_string())
            }
            // The example encodes the aggregate input as a bare JSON array.
            "aggregate_results" => {
                let items = value.as_array().cloned().unwrap_or_default();
                let total_words: u64 = items
                    .iter()
                    .filter_map(|item| item["word_count"].as_u64())
                    .sum();
                let summaries: Vec<serde_json::Value> =
                    items.iter().map(|item| item["summary"].clone()).collect();
                Ok(json!({
                    "total_urls": items.len(),
                    "total_words": total_words,
                    "summaries": summaries
                })
                .to_string())
            }
            other => Err(format!("terminal:unknown activity {other}")),
        }
    }
}

#[tokio::test]
async fn data_pipeline_example_runs_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    let path = examples_root().join("data-pipeline/data-pipeline.aion");
    if !path.exists() {
        eprintln!(
            "skipping data_pipeline_example_runs_end_to_end: {} not built",
            path.display()
        );
        return Ok(());
    }
    let package = Package::load_from_bytes(std::fs::read(&path)?)?;
    let engine = EngineBuilder::new()
        .store(InMemoryStore::default())
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(PipelineDispatcher))
        .load_workflows(package)
        .build()
        .await?;

    let input = Payload::from_json(&json!({
        "urls": ["https://example.com/a", "https://example.com/b"]
    }))?;
    let handle = engine.start_workflow("data_pipeline", input).await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    let payload = result.map_err(|error| format!("pipeline failed: {error:?}"))?;

    // The entry encodes AggregateOutput to its JSON text and the engine
    // records that as the result payload.
    let output: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(output["total_urls"], json!(2));
    assert_eq!(output["total_words"], json!(12));
    assert_eq!(
        output["summaries"]
            .as_array()
            .map(std::vec::Vec::len)
            .unwrap_or_default(),
        2
    );

    engine.shutdown()?;
    Ok(())
}
