//! Example-archive regression tests.
//!
//! Every example package under `examples/` must load into the engine, and
//! the behavioral examples must run end-to-end on the paths remote
//! deployments exercise: the data-pipeline fan-out/fan-in over activities,
//! the approval-gate signal race inside `with_timeout`, and the
//! subscription's billing deadline plus continue-as-new rotation. Archives
//! are build artifacts, so each test skips (with a notice) when its archive
//! has not been produced yet.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aion::activity::bridge::ActivityDispatcher;
use aion::signal::ConcreteSignalRouter;
use aion::{EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{Event, Payload};
use aion_package::{ExtractionLimits, Package};
use aion_store::{EventStore, InMemoryStore};
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
    (
        "order-fulfillment",
        "order-fulfillment/order-fulfillment.aion",
    ),
    ("order-shipping", "order-fulfillment/order-shipping.aion"),
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
        let package =
            Package::load_from_bytes(std::fs::read(&path)?, ExtractionLimits::unbounded())?;
        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .scheduler_threads(1)
            .load_workflows(package)
            .build()
            .await
            .map_err(|error| format!("{name} failed to load: {error}"))?;
        assert!(
            !engine.workflow_catalog().workflows()?.is_empty(),
            "{name} loaded no workflow versions"
        );
        engine.shutdown()?;
    }
    Ok(())
}

struct PipelineDispatcher;

impl ActivityDispatcher for PipelineDispatcher {
    fn dispatch(
        &self,
        name: &str,
        input: &str,
        _config: &str,
        _attempt: u32,
    ) -> Result<String, String> {
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
    let package = Package::load_from_bytes(std::fs::read(&path)?, ExtractionLimits::unbounded())?;
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
    let handle = engine
        .start_workflow("data_pipeline", input, std::collections::HashMap::new())
        .await?;
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

/// Records every dispatched activity and answers from the request fields —
/// the document actions for the approval gate and the invoice for the
/// subscription's billing cycle.
struct RecordingDispatcher {
    calls: Mutex<Vec<(String, serde_json::Value)>>,
}

impl RecordingDispatcher {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, serde_json::Value)> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

impl ActivityDispatcher for RecordingDispatcher {
    fn dispatch(
        &self,
        name: &str,
        input: &str,
        _config: &str,
        _attempt: u32,
    ) -> Result<String, String> {
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|e| format!("terminal:bad input: {e}"))?;
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((name.to_owned(), value.clone()));
        }
        match name {
            "publish_document" => Ok(json!({
                "action_taken": format!(
                    "published {}",
                    value["document_id"].as_str().unwrap_or_default()
                )
            })
            .to_string()),
            "archive_document" => Ok(json!({
                "action_taken": format!(
                    "archived {}",
                    value["document_id"].as_str().unwrap_or_default()
                )
            })
            .to_string()),
            "bill_subscriber" => Ok(json!({
                "subscriber_id": value["subscriber_id"],
                "plan": value["plan"],
                "cycle": value["cycle"],
                "invoice_id": format!(
                    "inv-{}-{}",
                    value["subscriber_id"].as_str().unwrap_or_default(),
                    value["cycle"]
                ),
                "status": "billed"
            })
            .to_string()),
            other => Err(format!("terminal:unknown activity {other}")),
        }
    }
}

#[tokio::test]
async fn approval_gate_signal_drives_publication() -> Result<(), Box<dyn std::error::Error>> {
    let path = examples_root().join("approval-gate/approval-gate.aion");
    if !path.exists() {
        eprintln!(
            "skipping approval_gate_signal_drives_publication: {} not built",
            path.display()
        );
        return Ok(());
    }
    let package = Package::load_from_bytes(std::fs::read(&path)?, ExtractionLimits::unbounded())?;
    let dispatcher = Arc::new(RecordingDispatcher::new());
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>)
        .load_workflows(package)
        .build()
        .await?;

    let input = Payload::from_json(&json!({
        "document_id": "doc-7",
        "timeout_minutes": 5
    }))?;
    let handle = engine
        .start_workflow("approval_gate", input, std::collections::HashMap::new())
        .await?;

    // The workflow arms its deadline and suspends in the signal receive; the
    // approval decision resolves the with_timeout race in the signal's favor.
    tokio::time::sleep(Duration::from_millis(300)).await;
    engine
        .signal(
            handle.workflow_id(),
            handle.run_id(),
            "approval_decision",
            Payload::from_json(&json!({ "decision": "approved" }))?,
        )
        .await?;

    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    let history = store.read_history(handle.workflow_id()).await?;
    let payload = result
        .map_err(|error| format!("approval gate failed: {error:?}\nhistory: {history:#?}"))?;
    let output: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(
        output["decision"],
        json!("approved"),
        "history: {history:#?}"
    );
    assert_eq!(output["action_taken"], json!("published doc-7"));

    let calls = dispatcher.calls();
    assert_eq!(
        calls
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["publish_document"],
        "approval must publish exactly once and never archive: {calls:?}"
    );

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn subscription_bills_after_deadline_with_signaled_plan_and_rotates()
-> Result<(), Box<dyn std::error::Error>> {
    let path = examples_root().join("subscription/subscription.aion");
    if !path.exists() {
        eprintln!(
            "skipping subscription_bills_after_deadline_with_signaled_plan_and_rotates: {} not built",
            path.display()
        );
        return Ok(());
    }
    let package = Package::load_from_bytes(std::fs::read(&path)?, ExtractionLimits::unbounded())?;
    let dispatcher = Arc::new(RecordingDispatcher::new());
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>)
        .load_workflows(package)
        .build()
        .await?;

    // One-second billing period, rotation after every cycle: the run waits
    // out its deadline, bills, and continues as new.
    let input = Payload::from_json(&json!({
        "subscriber_id": "sub-1",
        "subscriber_email": "sub-1@example.com",
        "plan": "basic",
        "current_cycle": 1,
        "billing_period_seconds": 1,
        "max_cycles": 1,
        "cycles_in_run": 0
    }))?;
    let handle = engine
        .start_workflow("subscription", input, std::collections::HashMap::new())
        .await?;

    // A plan change lands mid-period: the signal wins the with_timeout race
    // (recorded TimerCancelled), the wait resumes for the remaining period,
    // and the eventual billing must use the upgraded plan.
    tokio::time::sleep(Duration::from_millis(250)).await;
    engine
        .signal(
            handle.workflow_id(),
            handle.run_id(),
            "plan_change",
            Payload::from_json(&json!({ "direction": "upgrade", "plan": "pro" }))?,
        )
        .await?;

    // The first run ends in continue-as-new once the deadline fires and the
    // cycle is billed.
    let mut history = Vec::new();
    for _ in 0..120 {
        history = store.read_history(handle.workflow_id()).await?;
        if history
            .iter()
            .any(|event| matches!(event, Event::WorkflowContinuedAsNew { .. }))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::WorkflowContinuedAsNew { .. })),
        "subscription run never rotated via continue-as-new: {history:?}"
    );

    // The signal settled its enclosing scope as cancelled and the billing
    // deadline fired — both with_timeout outcomes appear durably.
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::TimerCancelled { .. })),
        "the signal-won with_timeout scope must record TimerCancelled: {history:?}"
    );
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::TimerFired { .. })),
        "the billing deadline must record TimerFired: {history:?}"
    );

    let calls = dispatcher.calls();
    let billing = calls
        .iter()
        .find(|(name, _)| name == "bill_subscriber")
        .ok_or_else(|| format!("bill_subscriber was never dispatched: {calls:?}"))?;
    assert_eq!(
        billing.1["plan"],
        json!("pro"),
        "billing must reflect the signaled upgrade: {calls:?}"
    );
    assert_eq!(billing.1["cycle"], json!(1));

    engine.shutdown()?;
    Ok(())
}
