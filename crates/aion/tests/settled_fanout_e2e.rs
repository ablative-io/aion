//! Settled fan-out end-to-end proofs (flow-vocab B4, memo §5 risk 1): N
//! outstanding SINGLE-dispatch activities awaited individually — the wire
//! `workflow.map_settled`/`all_settled` ride — against a real engine over a
//! shared `InMemoryStore`:
//!
//! 1. completion order INVERTED against item order: completions buffer in
//!    the keyed runtime maps and the awaits still resolve per correlation
//!    id, slots in item order;
//! 2. the workflow is parked on the item-zero await while later completions
//!    buffer, proving keyed delivery rather than head-of-line loss;
//! 3. one member failing terminally: the failure arrives as that slot's value
//!    — no fail-fast, no sibling cancellation, siblings keep their results;
//! 4. replay mid-settle: the engine restarts after only the item-zero prefix
//!    is terminal while later correlations remain outstanding; replay performs
//!    zero prefix re-dispatch, reopens exactly the unresolved members, and
//!    leaves the durable prefix byte-identical.

#[path = "test_support/gleam.rs"]
mod gleam_test_support;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{Event, Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

const SETTLED_MODULE: &str = "aion_settled_fixture";
const SETTLED_SOURCE: &[u8] =
    include_bytes!("fixtures/settled_gleam/src/aion_settled_fixture.gleam");

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_DEADLINE: Duration = Duration::from_secs(20);

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Named gates the test releases to settle individual fan-out members, plus
/// a finished-dispatch counter so tests can prove every dispatcher task
/// unblocked (and that replay never re-dispatched a recorded member).
struct GateBoard {
    released: Mutex<HashSet<String>>,
    condvar: Condvar,
    finished: AtomicUsize,
}

impl GateBoard {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            released: Mutex::new(HashSet::new()),
            condvar: Condvar::new(),
            finished: AtomicUsize::new(0),
        })
    }

    fn release(&self, key: &str) {
        if let Ok(mut released) = self.released.lock() {
            released.insert(key.to_owned());
            self.condvar.notify_all();
        }
    }

    fn wait(&self, key: &str) -> Result<(), String> {
        let deadline = std::time::Instant::now() + POLL_DEADLINE;
        let mut released = self
            .released
            .lock()
            .map_err(|_| "gate board lock poisoned".to_owned())?;
        while !released.contains(key) {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or_else(|| format!("gate {key} was never released"))?;
            let (guard, _) = self
                .condvar
                .wait_timeout(released, remaining)
                .map_err(|_| "gate board lock poisoned".to_owned())?;
            released = guard;
        }
        Ok(())
    }

    fn finished_dispatches(&self) -> usize {
        self.finished.load(Ordering::Acquire)
    }
}

/// Dispatcher for the fixture's gate protocol: `gated_ok:K` blocks on gate
/// `K` then succeeds with `"done-K"`; `gated_fail:K` blocks then fails with
/// `boom-K`.
struct GatedDispatcher {
    gates: Arc<GateBoard>,
}

impl ActivityDispatcher for GatedDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let name = request.name.as_str();
        let result = if let Some(key) = name.strip_prefix("gated_ok:") {
            self.gates.wait(key).map(|()| format!("\"done-{key}\""))
        } else if let Some(key) = name.strip_prefix("gated_fail:") {
            self.gates
                .wait(key)
                .and_then(|()| Err(format!("boom-{key}")))
        } else {
            Err(format!("unknown fixture activity {name}"))
        };
        self.gates.finished.fetch_add(1, Ordering::AcqRel);
        result
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/settled_gleam")
}

fn fixture_beams() -> Result<Vec<BeamModule>, Box<dyn std::error::Error>> {
    let root = fixture_root();
    let output = Command::new("gleam")
        .arg("build")
        .current_dir(&root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "real settled fixture did not compile:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let mut beams = Vec::new();
    for package in fs::read_dir(root.join("build/dev/erlang"))? {
        let ebin = package?.path().join("ebin");
        if !ebin.is_dir() {
            continue;
        }
        for entry in fs::read_dir(ebin)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("beam") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or("fixture BEAM has no UTF-8 module name")?;
            // The engine owns the real native boundary; a dependency build also
            // emits its external-function declaration module, which packages
            // must never replace.
            if name == "aion_flow_ffi" {
                continue;
            }
            beams.push(BeamModule::new(name, fs::read(&path)?));
        }
    }
    if beams.is_empty() {
        return Err("compiled fixture produced no BEAM modules".into());
    }
    Ok(beams)
}

fn fixture_package(entry_function: &str) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(fixture_beams()?)?;
    let manifest = Manifest {
        entry_module: SETTLED_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Some(Duration::from_secs(30)),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(SETTLED_MODULE, SETTLED_SOURCE.to_vec())])
            .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

async fn engine_over(
    store: &Arc<dyn EventStore>,
    gates: &Arc<GateBoard>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(4)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .query_timeout(QUERY_TIMEOUT)
        .activity_dispatcher(Arc::new(GatedDispatcher {
            gates: Arc::clone(gates),
        }))
        .load_workflows(fixture_package("settled_three")?)
        .build()
        .await?)
}

async fn start_parent(engine: &Engine) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            SETTLED_MODULE,
            Payload::from_json(&json!({ "fixture": "input" }))?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

async fn wait_for_history<F>(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    description: &str,
    predicate: F,
) -> Result<Vec<Event>, Box<dyn std::error::Error>>
where
    F: Fn(&[Event]) -> bool,
{
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let history = store.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if std::time::Instant::now() > deadline {
            return Err(format!("timed out waiting for {description}: {history:#?}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for<F>(description: &str, predicate: F) -> TestResult
where
    F: Fn() -> bool,
{
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    while !predicate() {
        if std::time::Instant::now() > deadline {
            return Err(format!("timed out waiting for {description}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

fn terminal_count(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::ActivityCompleted { .. } | Event::ActivityFailed { .. }
            )
        })
        .count()
}

fn scheduled_count(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ActivityScheduled { .. }))
        .count()
}

/// `(ordinal, kind)` per activity terminal, in recorded order. Single awaits
/// deliberately record in item/await order even when the completion messages
/// arrived in the inverse order; the staged dispatcher counter proves the
/// actual completion order separately.
fn terminal_ordinals(history: &[Event]) -> Vec<(u64, &'static str)> {
    history
        .iter()
        .filter_map(|event| match event {
            Event::ActivityCompleted { activity_id, .. } => {
                Some((activity_id.sequence_position(), "completed"))
            }
            Event::ActivityFailed { activity_id, .. } => {
                Some((activity_id.sequence_position(), "failed"))
            }
            _ => None,
        })
        .collect()
}

fn result_json(payload: &Payload) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(payload.bytes())?)
}

/// The whole risk-1 choreography in one run: three members dispatch before
/// any await; the test releases their gates in INVERTED item order (c, b,
/// a) with `b` failing terminally; the recorded terminals land in
/// completion order while the slots come back in item order with the
/// failure captured as a value; then the engine restarts with the fan-out
/// settled and the run parked at the release gate, and replay resolves all
/// three awaits purely from the recorded per-ordinal terminals.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn settled_fanout_inverts_completion_captures_failure_and_replays() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    // All three members dispatch before any settles: the single-dispatch
    // wire fans out first, awaits after (the settled contract).
    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        scheduled_count(events) == 3
    })
    .await?;

    // Make only the item-order prefix durable. Members b and c remain
    // outstanding when the engine stops, which is the recovery boundary this
    // proof is specifically intended to exercise.
    gates.release("a");
    let prefix = wait_for_history(&store, &workflow_id, "item-zero terminal", |events| {
        terminal_count(events) == 1
    })
    .await?;
    assert_eq!(terminal_ordinals(&prefix), vec![(0, "completed")]);
    engine.shutdown()?;

    // Replay consumes a from history without dispatching it and reopens only
    // unresolved b and c. Complete those in inverse order (c before b).
    let recovery_gates = GateBoard::new();
    let recovered = engine_over(&store, &recovery_gates).await?;
    recovery_gates.release("c");
    wait_for("recovered c dispatcher completion", || {
        recovery_gates.finished_dispatches() == 1
    })
    .await?;
    recovery_gates.release("b");
    wait_for("recovered b dispatcher completion", || {
        recovery_gates.finished_dispatches() == 2
    })
    .await?;
    let result = recovered
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("settled parent failed: {error:?}"))?;

    let decoded = result_json(&result)?;
    let text = decoded
        .as_str()
        .ok_or("settled result must be a JSON string")?;
    let slots: Vec<&str> = text.split('|').collect();
    assert_eq!(slots.len(), 3, "three slots, item order: {text}");
    assert_eq!(slots[0], "ok=done-a", "slot a keeps its success: {text}");
    assert!(
        slots[1].starts_with("err=") && slots[1].contains("boom-b"),
        "slot b carries its captured terminal failure: {text}"
    );
    assert_eq!(slots[2], "ok=done-c", "slot c keeps its success: {text}");

    let final_history = store.read_history(&workflow_id).await?;
    assert_eq!(
        &final_history[..prefix.len()],
        &prefix[..],
        "replay must leave the durable item-order prefix byte-identical"
    );
    assert_eq!(
        terminal_ordinals(&final_history),
        vec![(0, "completed"), (1, "failed"), (2, "completed")],
        "reopened members must still settle in item order: {final_history:#?}"
    );
    assert_eq!(
        recovery_gates.finished_dispatches(),
        2,
        "recovery must dispatch only the two unresolved members"
    );
    recovered.shutdown()?;
    Ok(())
}

/// No fail-fast, no sibling cancellation: with only the FAILING member
/// released, its terminal records while both siblings stay in flight —
/// scheduled, started, neither cancelled nor resolved — and the run still
/// completes once they settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn settled_failure_never_cancels_outstanding_siblings() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        scheduled_count(events) == 3
    })
    .await?;

    // b finishes behind the outstanding a await. Its failure is buffered as
    // a value; it neither fails the workflow nor cancels a or c.
    gates.release("b");
    wait_for("b dispatcher completion", || {
        gates.finished_dispatches() == 1
    })
    .await?;
    let buffered_failure = store.read_history(&workflow_id).await?;
    assert_eq!(
        terminal_count(&buffered_failure),
        0,
        "b must remain buffered while a is awaited: {buffered_failure:#?}"
    );
    assert!(
        !buffered_failure
            .iter()
            .any(|event| matches!(event, Event::ActivityCancelled { .. })),
        "a buffered failure must cancel nothing: {buffered_failure:#?}"
    );

    gates.release("a");
    let after_failure = wait_for_history(&store, &workflow_id, "b failure consumed", |events| {
        terminal_count(events) == 2
    })
    .await?;
    assert_eq!(
        terminal_ordinals(&after_failure),
        vec![(0, "completed"), (1, "failed")],
        "a succeeds and b is captured while c remains outstanding: {after_failure:#?}"
    );
    assert!(
        !after_failure
            .iter()
            .any(|event| matches!(event, Event::ActivityCancelled { .. })),
        "consuming b's failure must not cancel c: {after_failure:#?}"
    );

    gates.release("c");
    wait_for_history(&store, &workflow_id, "fan-out settled", |events| {
        terminal_count(events) == 3
    })
    .await?;
    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("settled parent failed: {error:?}"))?;
    let decoded = result_json(&result)?;
    let text = decoded
        .as_str()
        .ok_or("settled result must be a JSON string")?;
    assert!(
        text.starts_with("ok=done-a|err=") && text.ends_with("|ok=done-c"),
        "slots keep item order around the captured failure: {text}"
    );
    engine.shutdown()?;
    Ok(())
}
