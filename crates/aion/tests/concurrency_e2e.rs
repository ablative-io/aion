//! Two-phase `collect_all`/`collect_race`/`collect_map` end-to-end tests.
//!
//! These tests drive the converted suspending collect natives — parallel
//! activity dispatch over the shared completion-task machinery, the pinned
//! ordinal base, fail-fast and first-settle settlement with durable
//! cancellation sets, `with_timeout` scope expiry over a fan-out, the D5
//! runtime completion-map drain, and the query pump at the collect yield
//! point — against real BEAM workflow fixtures over a shared
//! `InMemoryStore`. Replay proofs restart the engine mid-run (the collect
//! settled, the run gated on a `release` signal) and require the recovered
//! run's history to remain **byte-identical**: replay resolves the collect
//! purely from recorded terminals and appends nothing.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, EngineError, QueryError, RuntimeHandle, SignalRouter};
use aion_core::{Event, Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

const COLLECT_MODULE: &str = "aion_collect_fixture";
const COLLECT_BEAM: &[u8] = include_bytes!("fixtures/aion_collect_fixture.beam");
const COLLECT_SOURCE: &[u8] = include_bytes!("fixtures/aion_collect_fixture.erl");

/// Generous engine reply deadline for tests where queries must succeed.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for fixture handler registration (workflow code races the caller).
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(20);
/// Deadline for any polled engine-side condition.
const POLL_DEADLINE: Duration = Duration::from_secs(20);

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Named gates the test releases to settle individual fan-out members, plus
/// a finished-dispatch counter so tests can prove every dispatcher task
/// unblocked before engine shutdown.
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

    /// Block the dispatcher until `key` is released; a generous timeout
    /// keeps a buggy test from wedging the shared Tokio workers forever.
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
    let beams = BeamSet::new(vec![BeamModule::new(COLLECT_MODULE, COLLECT_BEAM)])?;
    let manifest = Manifest {
        entry_module: COLLECT_MODULE.to_owned(),
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
        PackageBuilder::with_source(manifest, beams, [(COLLECT_MODULE, COLLECT_SOURCE.to_vec())])
            .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

/// Engine over `store` with the collect fixture loaded at `entry` and the
/// gated dispatcher wired to `gates`.
async fn engine_over(
    store: &Arc<dyn EventStore>,
    entry: &str,
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
        .load_workflows(fixture_package(entry)?)
        .build()
        .await?)
}

fn parent_input() -> Result<Payload, Box<dyn std::error::Error>> {
    Ok(Payload::from_json(&json!({ "fixture": "input" }))?)
}

fn signal_payload(label: &str) -> Result<Payload, Box<dyn std::error::Error>> {
    Ok(Payload::from_json(&json!({ "label": label }))?)
}

async fn start_parent(engine: &Engine) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            COLLECT_MODULE,
            parent_input()?,
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

fn count_completed(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ActivityCompleted { .. }))
        .count()
}

fn count_failed(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ActivityFailed { .. }))
        .count()
}

fn count_cancelled(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ActivityCancelled { .. }))
        .count()
}

fn activity_event_count(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::ActivityScheduled { .. }
                    | Event::ActivityStarted { .. }
                    | Event::ActivityCompleted { .. }
                    | Event::ActivityFailed { .. }
                    | Event::ActivityCancelled { .. }
            )
        })
        .count()
}

fn result_json(payload: &Payload) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(payload.bytes())?)
}

/// Project a run history onto its deterministic shape — seq, kind,
/// identifiers, and recorded payload bytes; envelope timestamps and the
/// scope timer's absolute `fire_at` are necessarily run-specific.
fn run_shape(history: &[Event]) -> Vec<String> {
    history
        .iter()
        .map(|event| match event {
            Event::WorkflowStarted {
                envelope,
                workflow_type,
                input,
                ..
            } => format!(
                "{}|started|{workflow_type}|{}",
                envelope.seq,
                String::from_utf8_lossy(input.bytes())
            ),
            Event::ActivityScheduled {
                envelope,
                activity_id,
                activity_type,
                input,
                ..
            } => format!(
                "{}|sched|{}|{activity_type}|{}",
                envelope.seq,
                activity_id.sequence_position(),
                String::from_utf8_lossy(input.bytes())
            ),
            Event::ActivityStarted {
                envelope,
                activity_id,
                attempt,
            } => format!(
                "{}|astart|{}|{attempt}",
                envelope.seq,
                activity_id.sequence_position()
            ),
            Event::ActivityCompleted {
                envelope,
                activity_id,
                result,
                attempt,
            } => format!(
                "{}|acomp|{}|{}|{attempt}",
                envelope.seq,
                activity_id.sequence_position(),
                String::from_utf8_lossy(result.bytes())
            ),
            Event::ActivityFailed {
                envelope,
                activity_id,
                error,
                attempt,
            } => format!(
                "{}|afail|{}|{}|{attempt}",
                envelope.seq,
                activity_id.sequence_position(),
                error.message
            ),
            Event::ActivityCancelled {
                envelope,
                activity_id,
                attempt,
            } => format!(
                "{}|acancel|{}|{attempt}",
                envelope.seq,
                activity_id.sequence_position()
            ),
            Event::TimerStarted {
                envelope, timer_id, ..
            } => format!("{}|tstart|{timer_id:?}", envelope.seq),
            Event::TimerFired {
                envelope, timer_id, ..
            } => format!("{}|tfired|{timer_id:?}", envelope.seq),
            Event::TimerCancelled {
                envelope, timer_id, ..
            } => format!("{}|tcancel|{timer_id:?}", envelope.seq),
            Event::SignalReceived {
                envelope,
                name,
                payload,
            } => format!(
                "{}|signal|{name}|{}",
                envelope.seq,
                String::from_utf8_lossy(payload.bytes())
            ),
            Event::WorkflowCompleted { envelope, result } => format!(
                "{}|completed|{}",
                envelope.seq,
                String::from_utf8_lossy(result.bytes())
            ),
            other => format!("{}|unexpected|{other:?}", other.seq()),
        })
        .collect()
}

/// Query `name`, retrying while the fixture has not yet executed its
/// `register_query` call (registration is workflow code, racing the caller).
async fn query_when_registered(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    name: &str,
) -> Result<Payload, EngineError> {
    let deadline = std::time::Instant::now() + REGISTRATION_DEADLINE;
    loop {
        let outcome = engine.query(workflow_id, run_id, name).await;
        match outcome {
            Err(EngineError::Query(QueryError::UnknownQuery(_)))
                if std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            other => return other,
        }
    }
}

/// Send `release` and await the workflow result, asserting it decodes to
/// `expected`.
async fn release_and_finish(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    expected: &serde_json::Value,
) -> TestResult {
    engine
        .signal(workflow_id, run_id, "release", signal_payload("release")?)
        .await?;
    let result = engine
        .result(workflow_id, run_id)
        .await?
        .map_err(|error| format!("collect parent failed: {error:?}"))?;
    assert_eq!(&result_json(&result)?, expected);
    Ok(())
}

/// Restart the engine over `store` and prove replay byte-identity: the
/// recovered run resolves the settled collect purely from recorded events,
/// appending nothing before the release signal. Returns the final history.
async fn restart_replay_and_finish(
    store: &Arc<dyn EventStore>,
    entry: &str,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    settled: &[Event],
    expected: &serde_json::Value,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let gates = GateBoard::new();
    let recovered = engine_over(store, entry, &gates).await?;
    release_and_finish(&recovered, workflow_id, run_id, expected).await?;
    let final_history = store.read_history(workflow_id).await?;
    assert_eq!(
        &final_history[..settled.len()],
        settled,
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
        gates.finished_dispatches(),
        0,
        "replay must never re-dispatch a recorded activity"
    );
    recovered.shutdown()?;
    Ok(final_history)
}

// --- brief §4 item 5: query a parent parked in collect_all -------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_answers_while_parent_is_parked_in_collect_all() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, "queryable_all", &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    // The parent is parked inside collect_all: both members are scheduled
    // and gated, nothing has settled.
    let before = wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    assert_eq!(count_completed(&before), 0);

    // The engine answers the query through the pump at the collect yield
    // point, appending nothing.
    let reply = match query_when_registered(&engine, &workflow_id, &run_id, "state").await {
        Ok(reply) => reply,
        Err(error) => {
            let history = store.read_history(&workflow_id).await?;
            return Err(format!("query failed: {error:?}; parent history: {history:#?}").into());
        }
    };
    let value: serde_json::Value = serde_json::from_slice(reply.bytes())?;
    assert_eq!(value["answer"], 1);
    assert_eq!(
        store.read_history(&workflow_id).await?,
        before,
        "the query path must never append events"
    );

    // Sequential release keeps the recorded completion order deterministic.
    gates.release("a");
    wait_for_history(
        &store,
        &workflow_id,
        "first completion recorded",
        |events| count_completed(events) == 1,
    )
    .await?;
    gates.release("b");
    wait_for_history(
        &store,
        &workflow_id,
        "both completions recorded",
        |events| count_completed(events) == 2,
    )
    .await?;
    release_and_finish(
        &engine,
        &workflow_id,
        &run_id,
        &json!(["\"done-a\"", "\"done-b\""]),
    )
    .await?;
    let queried_history = store.read_history(&workflow_id).await?;
    engine.shutdown()?;

    // Control: identical inputs, releases, and signals, never queried.
    let control_store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let control_gates = GateBoard::new();
    let control = engine_over(&control_store, "queryable_all", &control_gates).await?;
    let (control_id, control_run) = start_parent(&control).await?;
    wait_for_history(&control_store, &control_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    control_gates.release("a");
    wait_for_history(
        &control_store,
        &control_id,
        "first completion recorded",
        |events| count_completed(events) == 1,
    )
    .await?;
    control_gates.release("b");
    wait_for_history(
        &control_store,
        &control_id,
        "both completions recorded",
        |events| count_completed(events) == 2,
    )
    .await?;
    release_and_finish(
        &control,
        &control_id,
        &control_run,
        &json!(["\"done-a\"", "\"done-b\""]),
    )
    .await?;
    let control_history = control_store.read_history(&control_id).await?;
    control.shutdown()?;

    assert_eq!(
        run_shape(&queried_history),
        run_shape(&control_history),
        "a queried run's history must be shape-identical to the never-queried control"
    );
    Ok(())
}

// --- queried collect + crash/replay determinism --------------------------------

/// Settle a parked two-member fan-out deterministically: release gate "a",
/// wait for its recorded completion, release "b", and return the settled
/// history (the run is then parked at the release-signal gate).
async fn settle_two_member_fanout(
    store: &Arc<dyn EventStore>,
    gates: &Arc<GateBoard>,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    gates.release("a");
    wait_for_history(store, workflow_id, "first completion recorded", |events| {
        count_completed(events) == 1
    })
    .await?;
    gates.release("b");
    wait_for_history(store, workflow_id, "both completions recorded", |events| {
        count_completed(events) == 2
    })
    .await
}

/// Run one never-queried, never-crashed `queryable_all` control to
/// completion with the deterministic release order and return its history.
async fn unqueried_uncrashed_control_history() -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let control = engine_over(&store, "queryable_all", &gates).await?;
    let (workflow_id, run_id) = start_parent(&control).await?;
    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    settle_two_member_fanout(&store, &gates, &workflow_id).await?;
    release_and_finish(
        &control,
        &workflow_id,
        &run_id,
        &json!(["\"done-a\"", "\"done-b\""]),
    )
    .await?;
    let history = store.read_history(&workflow_id).await?;
    control.shutdown()?;
    Ok(history)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queried_collect_crash_recovery_matches_unqueried_uncrashed_control() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, "queryable_all", &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    // Query while the parent is parked inside collect_all (both members
    // gated, nothing settled): answered at the collect yield point with
    // byte-identical history before and after.
    let parked = wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    let reply = query_when_registered(&engine, &workflow_id, &run_id, "state").await?;
    let value: serde_json::Value = serde_json::from_slice(reply.bytes())?;
    assert_eq!(value["answer"], 1);
    assert_eq!(
        store.read_history(&workflow_id).await?,
        parked,
        "queries at the collect yield point must never append events"
    );

    // Settle the fan-out deterministically, then crash with the run parked
    // at the release gate: the recorded terminals are replay's only truth.
    let settled = settle_two_member_fanout(&store, &gates, &workflow_id).await?;
    engine.shutdown()?;

    // Recovery resolves the settled collect purely from history, appends
    // nothing, answers queries again (organic replay re-registration), and
    // never re-dispatches a recorded activity.
    let recovery_gates = GateBoard::new();
    let recovered = engine_over(&store, "queryable_all", &recovery_gates).await?;
    let reply = query_when_registered(&recovered, &workflow_id, &run_id, "state").await?;
    let value: serde_json::Value = serde_json::from_slice(reply.bytes())?;
    assert_eq!(value["answer"], 1);
    assert_eq!(
        store.read_history(&workflow_id).await?,
        settled,
        "neither recovery replay nor queries may append or rewrite events"
    );
    release_and_finish(
        &recovered,
        &workflow_id,
        &run_id,
        &json!(["\"done-a\"", "\"done-b\""]),
    )
    .await?;
    let queried_crashed = store.read_history(&workflow_id).await?;
    assert_eq!(
        &queried_crashed[..settled.len()],
        &settled[..],
        "replay must leave the settled prefix byte-identical"
    );
    assert_eq!(
        recovery_gates.finished_dispatches(),
        0,
        "replay must never re-dispatch a recorded activity"
    );
    recovered.shutdown()?;

    // Control: identical run and release order, never queried, never crashed.
    let control_history = unqueried_uncrashed_control_history().await?;
    assert_eq!(
        run_shape(&queried_crashed),
        run_shape(&control_history),
        "queried/crashed and unqueried/uncrashed histories must agree in shape"
    );
    Ok(())
}

// --- brief §4 item 9: all-success ordering + restart replay -------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collect_all_success_returns_input_order_and_replays_byte_identically() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, "all_two", &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    gates.release("a");
    wait_for_history(
        &store,
        &workflow_id,
        "first completion recorded",
        |events| count_completed(events) == 1,
    )
    .await?;
    gates.release("b");
    let settled = wait_for_history(&store, &workflow_id, "fan-out settled", |events| {
        count_completed(events) == 2
    })
    .await?;
    // Contiguous batch scheduling, then completions: the exact recorded
    // shape is pinned (the result list is input-ordered regardless).
    let shape = run_shape(&settled);
    assert_eq!(shape[1], format!("2|sched|0|gated_ok:a|\"in\""));
    assert_eq!(shape[2], "3|astart|0|1");
    assert_eq!(shape[3], format!("4|sched|1|gated_ok:b|\"in\""));
    assert_eq!(shape[4], "5|astart|1|1");
    assert_eq!(shape[5], "6|acomp|0|\"done-a\"|1");
    assert_eq!(shape[6], "7|acomp|1|\"done-b\"|1");
    wait_for("dispatcher tasks to finish", || {
        gates.finished_dispatches() == 2
    })
    .await?;
    engine.shutdown()?;

    // Restart: replay resolves the settled collect from recorded events
    // only and the run completes with the same input-ordered list.
    restart_replay_and_finish(
        &store,
        "all_two",
        &workflow_id,
        &run_id,
        &settled,
        &json!(["\"done-a\"", "\"done-b\""]),
    )
    .await?;
    Ok(())
}

// --- brief §4 item 9: fail-fast + cancellations + restart replay --------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collect_all_fail_fast_cancels_unresolved_and_replays_byte_identically() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, "all_fail_fast", &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    // Only the failing member settles; the other is cancelled durably.
    gates.release("b");
    let settled = wait_for_history(&store, &workflow_id, "fail-fast settled", |events| {
        count_failed(events) == 1 && count_cancelled(events) == 1
    })
    .await?;
    let shape = run_shape(&settled);
    assert_eq!(shape[5], "6|afail|1|boom-b|1");
    assert_eq!(shape[6], "7|acancel|0|1");
    assert_eq!(count_completed(&settled), 0);

    // Release the cancelled member's gate so its dispatcher task finishes;
    // its late completion must append nothing.
    gates.release("a");
    wait_for("dispatcher tasks to finish", || {
        gates.finished_dispatches() == 2
    })
    .await?;
    wait_for("late loser completion to land in the runtime maps", || {
        engine.runtime().retained_activity_completions() > 0
    })
    .await?;
    assert_eq!(
        store.read_history(&workflow_id).await?,
        settled,
        "a cancelled member's late completion must never record an event"
    );
    engine.shutdown()?;

    restart_replay_and_finish(
        &store,
        "all_fail_fast",
        &workflow_id,
        &run_id,
        &settled,
        &json!("boom-b"),
    )
    .await?;
    Ok(())
}

// --- brief §4 item 9: race first-settle success + restart replay --------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collect_race_settles_first_success_and_replays_byte_identically() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, "race_two", &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    // The second member settles first and wins; the first is cancelled.
    gates.release("b");
    let settled = wait_for_history(&store, &workflow_id, "race settled", |events| {
        count_completed(events) == 1 && count_cancelled(events) == 1
    })
    .await?;
    let shape = run_shape(&settled);
    assert_eq!(shape[5], "6|acomp|1|\"done-b\"|1");
    assert_eq!(shape[6], "7|acancel|0|1");

    gates.release("a");
    wait_for("dispatcher tasks to finish", || {
        gates.finished_dispatches() == 2
    })
    .await?;
    assert_eq!(
        store.read_history(&workflow_id).await?,
        settled,
        "the loser's late completion must never record an event"
    );
    engine.shutdown()?;

    restart_replay_and_finish(
        &store,
        "race_two",
        &workflow_id,
        &run_id,
        &settled,
        &json!("done-b"),
    )
    .await?;
    Ok(())
}

// --- brief §4 item 9: race first-settle failure + restart replay --------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collect_race_settles_first_failure_and_replays_byte_identically() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, "race_fail", &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    // First settle is a failure: per the SDK contract the failure wins the
    // race; the still-running member is cancelled.
    gates.release("a");
    let settled = wait_for_history(&store, &workflow_id, "race settled on failure", |events| {
        count_failed(events) == 1 && count_cancelled(events) == 1
    })
    .await?;
    let shape = run_shape(&settled);
    assert_eq!(shape[5], "6|afail|0|boom-a|1");
    assert_eq!(shape[6], "7|acancel|1|1");
    assert_eq!(count_completed(&settled), 0);

    gates.release("b");
    wait_for("dispatcher tasks to finish", || {
        gates.finished_dispatches() == 2
    })
    .await?;
    assert_eq!(
        store.read_history(&workflow_id).await?,
        settled,
        "the loser's late completion must never record an event"
    );
    engine.shutdown()?;

    restart_replay_and_finish(
        &store,
        "race_fail",
        &workflow_id,
        &run_id,
        &settled,
        &json!("boom-a"),
    )
    .await?;
    Ok(())
}

// --- brief §4 item 9 + D5: race losers leak nothing past process exit ---------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn race_loser_late_completion_records_nothing_and_monitor_drains_the_maps() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, "race_two", &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        activity_event_count(events) == 4
    })
    .await?;
    gates.release("a");
    let settled = wait_for_history(&store, &workflow_id, "race settled", |events| {
        count_completed(events) == 1 && count_cancelled(events) == 1
    })
    .await?;

    // The loser settles late while the parent is parked on the release
    // gate: its completion lands in the runtime maps (no event recorded)
    // and stays there — the workflow never takes it.
    gates.release("b");
    wait_for("late loser completion to land in the runtime maps", || {
        engine.runtime().retained_activity_completions() > 0
    })
    .await?;
    assert_eq!(
        store.read_history(&workflow_id).await?,
        settled,
        "the loser's late completion must never record an event"
    );

    // Completing the run exits the workflow process; the monitor drain (D5)
    // must remove the leaked entry with it.
    release_and_finish(&engine, &workflow_id, &run_id, &json!("done-a")).await?;
    wait_for(
        "monitor drain to clear the retained completion maps",
        || engine.runtime().retained_activity_completions() == 0,
    )
    .await?;
    engine.shutdown()?;
    Ok(())
}

// --- brief §4 item 9: collect under with_timeout expiry + restart replay ------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collect_under_with_timeout_expiry_cancels_all_and_replays_byte_identically() -> TestResult
{
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = GateBoard::new();
    let engine = engine_over(&store, "all_timeout", &gates).await?;
    let (workflow_id, run_id) = start_parent(&engine).await?;

    // Nothing is ever released before the deadline: the scope timer fires
    // and the collect aborts with every member cancelled durably.
    let settled = wait_for_history(&store, &workflow_id, "scope-expired fan-out", |events| {
        count_cancelled(events) == 2
            && events
                .iter()
                .any(|event| matches!(event, Event::TimerFired { .. }))
    })
    .await?;
    assert_eq!(count_completed(&settled), 0);
    assert_eq!(count_failed(&settled), 0);

    // Unblock the dispatcher tasks; their late completions are deliveries
    // for cancelled ordinals and must append nothing.
    gates.release("a");
    gates.release("b");
    wait_for("dispatcher tasks to finish", || {
        gates.finished_dispatches() == 2
    })
    .await?;
    wait_for("late completions to land in the runtime maps", || {
        engine.runtime().retained_activity_completions() == 2
    })
    .await?;
    assert_eq!(
        store.read_history(&workflow_id).await?,
        settled,
        "late completions for scope-cancelled members must never record events"
    );
    engine.shutdown()?;

    // Restart: with_timeout replays its recorded TimerFired, the collect
    // sweep derives the same abort from the cancelled-without-failure set,
    // and the run completes with the fixture's timeout marker.
    restart_replay_and_finish(
        &store,
        "all_timeout",
        &workflow_id,
        &run_id,
        &settled,
        &json!("timed_out"),
    )
    .await?;
    Ok(())
}
