//! Example-archive regression tests.
//!
//! Every example package under `examples/` must load into the engine, and
//! the behavioral examples must run end-to-end on the paths remote
//! deployments exercise: the data-pipeline fan-out/fan-in over activities,
//! the approval-gate signal race inside `with_timeout`, and the
//! subscription's billing deadline plus continue-as-new rotation. Every
//! archive is rebuilt from the committed example source on each run — see
//! `common/example_build.rs` for why these gates must never skip.

#[path = "test_support/gleam.rs"]
mod gleam_test_support;

#[path = "common/example_build.rs"]
mod example_build;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{Event, Payload};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

const EXAMPLE_PROJECTS: &[&str] = &[
    "approval-gate",
    "batch-orchestrator",
    "data-pipeline",
    "subscription",
    "agent-orchestration",
    "order-saga",
    "order-fulfillment",
    "hello-world",
    "retry-policy",
    "stacked-dev",
    "invm-demo",
];

#[tokio::test]
async fn every_example_archive_loads_into_the_engine() -> Result<(), Box<dyn std::error::Error>> {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    for name in EXAMPLE_PROJECTS {
        let report = example_build::build_project(&format!("examples/{name}"))?;
        assert!(
            !report.packages.is_empty(),
            "{name} declared no workflow packages"
        );
        let mut builder = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .scheduler_threads(1);
        for packaged in &report.packages {
            builder = builder.load_workflows(packaged.package.clone());
        }
        let engine = builder
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

/// The in-VM demo runs end-to-end with NO activity dispatcher configured:
/// its single `shout` activity is `execution_tier(InVm)`, so the runner
/// executes in a linked child process inside the engine — the whole point of
/// the example is that no worker (and no dispatcher seam) exists.
#[tokio::test]
async fn invm_demo_example_runs_end_to_end_without_a_dispatcher()
-> Result<(), Box<dyn std::error::Error>> {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let package = example_build::built_package("examples/invm-demo", "invm_demo")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?;

    let input = Payload::from_json(&json!({ "name": "sydney" }))?;
    let handle = engine
        .start_workflow(
            "invm_demo",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    let payload = result.map_err(|error| format!("invm-demo failed: {error:?}"))?;
    let output: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(output, json!("SYDNEY!!!"));

    // The in-VM dispatch recorded remote-shaped history: Scheduled, Started,
    // and Completed for the single `shout` activity.
    let history = store.read_history(handle.workflow_id()).await?;
    assert!(history.iter().any(|event| matches!(
        event,
        Event::ActivityScheduled { activity_type, .. } if activity_type == "shout"
    )));
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::ActivityCompleted { .. }))
    );

    engine.shutdown()?;
    Ok(())
}

struct PipelineDispatcher;

impl ActivityDispatcher for PipelineDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let name = request.name.as_str();
        let input = request.input.as_str();
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
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let package = example_build::built_package("examples/data-pipeline", "data_pipeline")?;
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
        .start_workflow(
            "data_pipeline",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
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
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let name = request.name.as_str();
        let input = request.input.as_str();
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
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let package = example_build::built_package("examples/approval-gate", "approval_gate")?;
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
        .start_workflow(
            "approval_gate",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
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
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let package = example_build::built_package("examples/subscription", "subscription")?;
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
        .start_workflow(
            "subscription",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
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

/// Answers the gate's one activity with a passing verdict in the
/// `gate_result_codec` wire shape.
struct GateDispatcher;

impl ActivityDispatcher for GateDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        match request.name.as_str() {
            "full_checks" => Ok(json!({ "verdict": { "outcome": "pass" } }).to_string()),
            other => Err(format!("terminal:unknown activity {other}")),
        }
    }
}

/// A valid gate start input in the `gate_input_codec` wire shape.
fn gate_input() -> serde_json::Value {
    json!({
        "workspace": {
            "path": "/tmp/ws",
            "branch": "main",
            "placement": "local",
            "isolation": "worktree"
        },
        "files_touched": ["src/lib.rs"],
        "scope": { "kind": "workspace_wide" }
    })
}

fn gate_engine_builder(dispatcher: Arc<dyn ActivityDispatcher>) -> EngineBuilder {
    EngineBuilder::new()
        .store(InMemoryStore::default())
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(dispatcher)
}

/// Live proof for the `workflow.entrypoint` migration: the stacked-dev gate's
/// engine entry is now the one-line `workflow.entrypoint(definition(), raw)`
/// shim, and the recorded completion payload must be exactly what the
/// hand-written adapter produced — the `gate_result_codec` encoding of the
/// activity's verdict, byte for byte.
#[tokio::test]
async fn stacked_dev_gate_completes_through_the_entrypoint_shim()
-> Result<(), Box<dyn std::error::Error>> {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let package = example_build::built_package("examples/stacked-dev", "gate")?;
    let engine = gate_engine_builder(Arc::new(GateDispatcher))
        .load_workflows(package)
        .build()
        .await?;

    let input = Payload::from_json(&gate_input())?;
    let handle = engine
        .start_workflow(
            "gate",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    let payload = result.map_err(|error| format!("gate failed: {error:?}"))?;

    // Byte-for-byte: `gate_result_codec().encode(GateResult(GatePass))` —
    // the exact payload the pre-migration hand-written adapter recorded.
    assert_eq!(
        std::str::from_utf8(payload.bytes())?,
        r#"{"verdict":{"outcome":"pass"}}"#,
    );

    engine.shutdown()?;
    Ok(())
}

/// The migrated garbage-input edge: an input the gate's codec rejects fails
/// the run with the SDK's documented `aion_error: input_decode` envelope as
/// the failure details (previously a hand-rolled `GateStageFailed`).
#[tokio::test]
async fn stacked_dev_gate_records_the_input_decode_envelope_on_garbage_input()
-> Result<(), Box<dyn std::error::Error>> {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let package = example_build::built_package("examples/stacked-dev", "gate")?;
    let engine = gate_engine_builder(Arc::new(GateDispatcher))
        .load_workflows(package)
        .build()
        .await?;

    let input = Payload::from_json(&json!({ "unexpected": true }))?;
    let handle = engine
        .start_workflow(
            "gate",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    let error = match result {
        Ok(payload) => {
            return Err(format!(
                "garbage input must fail the gate, got completion: {:?}",
                std::str::from_utf8(payload.bytes())
            )
            .into());
        }
        Err(error) => error,
    };

    let details = error
        .details
        .clone()
        .ok_or_else(|| format!("failure must carry details: {error:?}"))?;
    let envelope: serde_json::Value = serde_json::from_slice(details.bytes())?;
    assert_eq!(envelope["aion_error"], json!("input_decode"));
    assert!(
        envelope["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "envelope carries a decode reason: {envelope}"
    );
    assert!(
        envelope["path"].is_array(),
        "envelope carries the decode path: {envelope}"
    );

    engine.shutdown()?;
    Ok(())
}
