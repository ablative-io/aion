//! Settled fan-out end-to-end proofs (flow-vocab B4, memo §5 risk 1): N
//! outstanding SINGLE-dispatch activities awaited individually — the wire
//! `workflow.map_settled`/`all_settled` ride — against a real engine over a
//! shared `InMemoryStore`:
//!
//! 1. completion order INVERTED against item order: completions buffer in
//!    the keyed runtime maps and the awaits still resolve per correlation
//!    id, slots in item order;
//! 2. one member failing terminally: the failure arrives as that slot's
//!    value — no fail-fast, no sibling cancellation, siblings keep their
//!    results;
//! 3. replay mid-settle: the engine restarts after the fan-out's terminals
//!    are recorded but before the run completes; replay resolves every
//!    await purely from the recorded per-ordinal terminals (zero
//!    re-dispatch) and the settled prefix stays byte-identical.

use std::collections::HashSet;
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
const SETTLED_BEAM: &[u8] = include_bytes!("fixtures/aion_settled_fixture.beam");
const SETTLED_SOURCE: &[u8] = include_bytes!("fixtures/aion_settled_fixture.erl");

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

fn fixture_package(entry_function: &str) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(SETTLED_MODULE, SETTLED_BEAM)])?;
    let manifest = Manifest {
        entry_module: SETTLED_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
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
        .scheduler_threads(1)
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

/// `(ordinal, kind)` per activity terminal, in RECORDED order — the proof
/// hook for inverted completion order.
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settled_fanout_inverts_completion_captures_failure_and_replays() -> TestResult {
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

    // Invert completion order against item order: c first, then b (the
    // terminal failure), then a. Waiting for each terminal to RECORD
    // before releasing the next pins the recorded completion order.
    gates.release("c");
    wait_for_history(&store, &workflow_id, "c terminal recorded", |events| {
        terminal_count(events) == 1
    })
    .await?;
    gates.release("b");
    wait_for_history(&store, &workflow_id, "b terminal recorded", |events| {
        terminal_count(events) == 2
    })
    .await?;
    gates.release("a");
    let settled = wait_for_history(&store, &workflow_id, "fan-out settled", |events| {
        terminal_count(events) == 3
    })
    .await?;

    // Recorded terminals arrive in COMPLETION order (2, 1, 0) — the
    // inversion is real — and member 1 is the terminal failure.
    assert_eq!(
        terminal_ordinals(&settled),
        vec![(2, "completed"), (1, "failed"), (0, "completed")],
        "terminals must record in completion order: {settled:#?}"
    );
    wait_for("dispatcher tasks to finish", || {
        gates.finished_dispatches() == 3
    })
    .await?;
    engine.shutdown()?;

    // Replay mid-settle: restart with every terminal recorded but the run
    // still parked at the release gate. Replay resolves each await from
    // its recorded ordinal — zero re-dispatch — and the settled prefix
    // stays byte-identical.
    let recovery_gates = GateBoard::new();
    let recovered = engine_over(&store, &recovery_gates).await?;
    recovered
        .signal(
            &workflow_id,
            &run_id,
            "release",
            Payload::from_json(&json!({ "label": "release" }))?,
        )
        .await?;
    let result = recovered
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("settled parent failed: {error:?}"))?;

    // Slots in ITEM order (a, b, c) regardless of completion order, the
    // failure captured as slot b's value, siblings unharmed.
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
        &final_history[..settled.len()],
        &settled[..],
        "replay must leave the settled prefix byte-identical"
    );
    assert_eq!(
        final_history.len(),
        settled.len() + 2,
        "replay may append only the release signal and the terminal: {final_history:#?}"
    );
    assert!(matches!(
        final_history[settled.len()],
        Event::SignalReceived { .. }
    ));
    assert!(matches!(
        final_history[settled.len() + 1],
        Event::WorkflowCompleted { .. }
    ));
    assert_eq!(
        recovery_gates.finished_dispatches(),
        0,
        "replay must never re-dispatch a recorded member"
    );
    recovered.shutdown()?;
    Ok(())
}

/// No fail-fast, no sibling cancellation: with only the FAILING member
/// released, its terminal records while both siblings stay in flight —
/// scheduled, started, neither cancelled nor resolved — and the run still
/// completes once they settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settled_failure_never_cancels_outstanding_siblings() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        scheduled_count(events) == 3
    })
    .await?;
    gates.release("b");
    let after_failure =
        wait_for_history(&store, &workflow_id, "b terminal recorded", |events| {
            terminal_count(events) == 1
        })
        .await?;
    assert_eq!(
        terminal_ordinals(&after_failure),
        vec![(1, "failed")],
        "only the failing member settles: {after_failure:#?}"
    );
    assert!(
        !after_failure
            .iter()
            .any(|event| matches!(event, Event::ActivityCancelled { .. })),
        "a settled member's failure must cancel nothing: {after_failure:#?}"
    );

    gates.release("a");
    gates.release("c");
    wait_for_history(&store, &workflow_id, "fan-out settled", |events| {
        terminal_count(events) == 3
    })
    .await?;
    engine
        .signal(
            &workflow_id,
            &run_id,
            "release",
            Payload::from_json(&json!({ "label": "release" }))?,
        )
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
