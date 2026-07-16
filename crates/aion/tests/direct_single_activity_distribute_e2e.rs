//! Real-engine proof for the DIRECT-compiled tolerant SINGLE-ACTIVITY
//! `distribute` — the combinator (`aion@workflow:map_settled/2`) track, which
//! spawns no implicit children. This is the shape whose archive previously
//! crashed the VM with `undef aion@workflow:map_settled/2` when the embedded
//! SDK closure lagged the lowering: the branch's earlier tests proved the
//! archive SHAPE (no synthesized children) but never executed the combinator
//! at runtime. Here the archive runs to completion on a real engine with
//! per-slot Option semantics: one member fails terminally, its slot stays a
//! slot (no fail-fast, no compaction, no sibling cancellation), and the
//! collected count still covers every input item.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{EngineBuilder, RuntimeHandle, SignalRouter};
use aion_awl_package::compile_and_assemble_awl;
use aion_core::{Event, Payload};
use aion_package::{ExtractionLimits, Package};
use aion_store::{EventStore, InMemoryStore};
use serde_json::{Value, json};

const MODULE: &str = "single_tol_probe";

/// The panel's `single_tol` scenario: a tolerant collect over a
/// single-activity distribute track.
const AWL: &str = r#"//! Tolerant single-activity distribute: the map_settled combinator track.
workflow single_tol_probe
  input items: [String]
  outcome done: type Done, route success

type Done { total: Int }

worker proof
  action score(item: String) -> String

step fan
  distribute item in items

step only
  score(item: item) -> result

step gather
  collect result? -> results
  results |> count -> total
  route done(total: total)
"#;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Fails item `b` terminally; every other item completes immediately.
struct FailBDispatcher;

impl ActivityDispatcher for FailBDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        if request.name != "score" {
            return Err(format!("unknown proof activity {}", request.name));
        }
        let value: Value =
            serde_json::from_str(&request.input).map_err(|error| error.to_string())?;
        let item = value
            .get("item")
            .and_then(Value::as_str)
            .ok_or_else(|| "score input has no string item".to_owned())?;
        if item == "b" {
            Err("intentional-b".to_owned())
        } else {
            serde_json::to_string(&format!("{item}-scored")).map_err(|error| error.to_string())
        }
    }
}

/// Tolerant single-activity distribute runs the DIRECT archive to
/// completion: the failed member holds a slot (count covers all items), the
/// parent never fails, no child workflow exists, and no cancellation is
/// recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_single_activity_tolerant_distribute_completes_with_slot_semantics() -> TestResult {
    let prepared = compile_and_assemble_awl(AWL, Path::new("."))?;
    assert!(
        prepared.compiled.synthesized_workflows.is_empty(),
        "single-activity distribute must stay a combinator fan-out"
    );
    let package = Package::load_from_bytes(prepared.archive, ExtractionLimits::unbounded())?;

    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(2)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::new(FailBDispatcher))
        .load_workflows(package)
        .build()
        .await?;
    let handle = engine
        .start_workflow(
            MODULE,
            Payload::from_json(&json!({ "items": ["a", "b", "c"] }))?,
            std::collections::HashMap::new(),
            "default".to_owned(),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();

    let result = tokio::time::timeout(
        Duration::from_secs(20),
        engine.result(&workflow_id, &run_id),
    )
    .await??
    .map_err(|error| format!("tolerant parent must complete, failed: {error:?}"))?;
    let decoded: Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(decoded.get("outcome").and_then(Value::as_str), Some("done"));
    // Per-slot Option semantics: the failed member's slot is ABSENT data but
    // still a slot — the tolerant collect neither fails fast nor compacts,
    // so the count covers all three items.
    assert_eq!(
        decoded.pointer("/payload/total").and_then(Value::as_i64),
        Some(3),
        "tolerant collect must preserve one slot per item: {decoded}"
    );

    let history = store.read_history(&workflow_id).await?;
    let scheduled = history
        .iter()
        .filter(|event| {
            matches!(event, Event::ActivityScheduled { activity_type, .. } if activity_type == "score")
        })
        .count();
    let completed = history
        .iter()
        .filter(|event| matches!(event, Event::ActivityCompleted { .. }))
        .count();
    let failed = history
        .iter()
        .filter(|event| matches!(event, Event::ActivityFailed { .. }))
        .count();
    assert_eq!(scheduled, 3, "one dispatch per item: {history:#?}");
    assert_eq!(completed, 2, "siblings keep their results: {history:#?}");
    assert_eq!(failed, 1, "exactly the b member fails: {history:#?}");
    assert!(
        !history.iter().any(|event| matches!(
            event,
            Event::ChildWorkflowStarted { .. } | Event::ChildWorkflowCancelled { .. }
        )),
        "the combinator track must spawn no children and cancel nothing"
    );
    engine.shutdown()?;
    Ok(())
}
