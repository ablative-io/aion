//! Multi-level child-workflow nesting end-to-end gates.
//!
//! Child workflows were proven one level deep (`child_workflows_e2e.rs`,
//! `child_await_e2e.rs`, the order saga); these tests prove the engine at
//! depth with the `nested_chain` fixture (`tests/fixtures/nested_chain/`),
//! rebuilt from committed Gleam source on every run:
//!
//! - `level_one` spawns `level_two`, which spawns `level_three`, which runs
//!   the `leaf_work` activity and answers a query — three full workflows,
//!   each with its own history, results propagating bottom-up.
//! - Queries are answered at the top (parked in `child.await`) and at the
//!   leaf (parked at the activity await) simultaneously, appending nothing.
//! - Recovery at depth: the engine is killed and rebuilt over the same
//!   store, both at a byte-stable signal park (all three histories replay
//!   byte-identically, zero respawns) and mid-activity (pinning today's
//!   at-least-once re-dispatch semantics).
//! - `level_self` spawns its own workflow type until a depth parameter
//!   stops it, proving recursion is unrestricted and recovers.
//! - Cancellation semantics (current, pending design): cancelling
//!   `level_two` records its `WorkflowCancelled` and kills its process
//!   only. The awaiting `level_one` gets a recorded `ChildWorkflowFailed`
//!   with the `cancelled:<reason>` message (the child-terminal watcher's
//!   D-4 mapping in `src/runtime/nif_child_watch.rs`), while `level_three`
//!   is silently orphaned: children are not process-linked to parents by
//!   design (`src/child/spawn.rs`), and no parent-close policy exists yet.
//!
//! Every spawn input, activity input, activity result, and the release
//! signal payload is deliberately >64 bytes so the large refc-binary path
//! stays exercised end-to-end (the recovery test's propagated outputs
//! carry the >64-byte release token too; the short `l1:l2:l3:...` result
//! strings elsewhere intentionally cover the small-payload path).

#[path = "common/example_build.rs"]
mod example_build;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use aion::activity::bridge::ActivityDispatcher;
use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, EngineError, QueryError, RuntimeHandle, SignalRouter};
use aion_core::{Event, Payload, RunId, WorkflowId, WorkflowStatus, status_from_events};
use aion_package::Package;
use aion_store::{EventStore, InMemoryStore};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FIXTURE: &str = "crates/aion/tests/fixtures/nested_chain";
/// A deliberately large (~250-byte) note riding every spawn input and
/// activity payload: beamr < 0.6.0 killed any workflow receiving a >64-byte
/// payload (refc-binary BIF defect, fixed upstream) — nesting must keep that
/// path honestly exercised at every level.
const NOTE: &str = "audit: nested-chain conformance probe; payload deliberately exceeds the \
sixty-four byte inline-binary boundary at every level so parent spawn inputs, activity \
inputs, and activity results all ride the large refc-binary path end-to-end across \
the whole nested chain";
/// A >64-byte release-signal token, for the same reason.
const RELEASE_TOKEN: &str = "release-credential: durable-resume authorized by the recovery \
gate after byte-identical replay was asserted across all three nested histories";

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A one-shot release gate the tests hold while a `leaf_work` dispatch is
/// in flight. Dispatch runs on the blocking pool, so a gated activity parks
/// its workflow at the activity-await yield point without wedging the
/// engine.
struct Gate {
    released: Mutex<bool>,
    condvar: Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            released: Mutex::new(false),
            condvar: Condvar::new(),
        })
    }

    fn release(&self) {
        match self.released.lock() {
            Ok(mut released) => {
                *released = true;
                self.condvar.notify_all();
            }
            Err(poisoned) => {
                let mut released = poisoned.into_inner();
                *released = true;
                self.condvar.notify_all();
            }
        }
    }

    fn wait(&self) -> Result<(), String> {
        let released = self
            .released
            .lock()
            .map_err(|_| "gate mutex poisoned".to_owned())?;
        let (_released, timeout) = self
            .condvar
            .wait_timeout_while(released, POLL_DEADLINE, |released| !*released)
            .map_err(|_| "gate mutex poisoned".to_owned())?;
        if timeout.timed_out() {
            return Err("gate wait timed out".to_owned());
        }
        Ok(())
    }
}

/// Deterministic in-process stand-in for the remote `leaf_work` worker,
/// mirroring the fixture's local stub: receipt `done:<job_id>`, audit
/// echoing the large note. Counts dispatch entries so the recovery tests
/// can prove (or pin) re-dispatch behavior exactly.
struct LeafDispatcher {
    gate: Option<Arc<Gate>>,
    dispatched: AtomicUsize,
}

impl LeafDispatcher {
    fn new(gate: Option<Arc<Gate>>) -> Arc<Self> {
        Arc::new(Self {
            gate,
            dispatched: AtomicUsize::new(0),
        })
    }

    fn dispatch_count(&self) -> usize {
        self.dispatched.load(Ordering::Acquire)
    }
}

impl ActivityDispatcher for LeafDispatcher {
    fn dispatch(
        &self,
        name: &str,
        input: &str,
        _config: &str,
        _attempt: u32,
    ) -> Result<String, String> {
        if name != "leaf_work" {
            return Err(format!("terminal:unknown fixture activity {name}"));
        }
        self.dispatched.fetch_add(1, Ordering::AcqRel);
        let value: Value =
            serde_json::from_str(input).map_err(|e| format!("terminal:bad input: {e}"))?;
        let job_id = value["job_id"]
            .as_str()
            .ok_or_else(|| "terminal:leaf_work input missing job_id".to_owned())?
            .to_owned();
        let note = value["note"]
            .as_str()
            .ok_or_else(|| "terminal:leaf_work input missing note".to_owned())?
            .to_owned();
        if let Some(gate) = &self.gate {
            gate.wait().map_err(|reason| format!("terminal:{reason}"))?;
        }
        Ok(json!({ "receipt": format!("done:{job_id}"), "audit": note }).to_string())
    }
}

/// Build all four fixture packages from the committed Gleam source. The
/// four `[[workflow]]` entries share one project: after the first call the
/// per-project flock plus `gleam build`'s incremental cache make the repeat
/// builds cheap.
fn chain_packages() -> Result<Vec<Package>, Box<dyn std::error::Error>> {
    Ok(vec![
        example_build::built_package(FIXTURE, "level_one")?,
        example_build::built_package(FIXTURE, "level_two")?,
        example_build::built_package(FIXTURE, "level_three")?,
        example_build::built_package(FIXTURE, "level_self")?,
    ])
}

async fn engine_over(
    store: &Arc<dyn EventStore>,
    dispatcher: &Arc<LeafDispatcher>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    let mut builder = EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .query_timeout(QUERY_TIMEOUT)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::clone(dispatcher) as Arc<dyn ActivityDispatcher>);
    for package in chain_packages()? {
        builder = builder.load_workflows(package);
    }
    Ok(builder.build().await?)
}

fn chain_input(job_id: &str, gate: bool) -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "job_id": job_id, "note": NOTE, "gate": gate }))
}

async fn start(
    engine: &Engine,
    workflow_type: &str,
    input: Payload,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(workflow_type, input, HashMap::new())
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
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let history = store.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {description}: {history:#?}").into());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the parent's history for its recorded `ChildWorkflowStarted` of
/// `child_type` and return the pre-allocated child workflow id.
async fn wait_for_child_started(
    store: &Arc<dyn EventStore>,
    parent_id: &WorkflowId,
    child_type: &str,
) -> Result<WorkflowId, Box<dyn std::error::Error>> {
    let description = format!("ChildWorkflowStarted({child_type}) in {parent_id}");
    let history = wait_for_history(store, parent_id, &description, |events| {
        child_started_id(events, child_type).is_some()
    })
    .await?;
    child_started_id(&history, child_type)
        .ok_or_else(|| format!("no ChildWorkflowStarted({child_type}) after wait").into())
}

fn child_started_id(history: &[Event], child_type: &str) -> Option<WorkflowId> {
    history.iter().find_map(|event| match event {
        Event::ChildWorkflowStarted {
            child_workflow_id,
            workflow_type,
            ..
        } if workflow_type == child_type => Some(child_workflow_id.clone()),
        _ => None,
    })
}

/// The run id recorded by a workflow's own `WorkflowStarted`.
fn run_id_of(history: &[Event]) -> Result<RunId, Box<dyn std::error::Error>> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .ok_or_else(|| "history has no WorkflowStarted".into())
}

/// Poll a query until the target has registered the handler (registration
/// is workflow code racing the caller after start/recovery).
async fn query_when_registered(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    name: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        match engine.query(workflow_id, run_id, name).await {
            Err(EngineError::Query(QueryError::UnknownQuery(_))) if Instant::now() < deadline => {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            outcome => {
                let payload = outcome?;
                return Ok(serde_json::from_slice(payload.bytes())?);
            }
        }
    }
}

async fn completed_result(
    engine: &Engine,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<Value, Box<dyn std::error::Error>> {
    let result = engine.result(workflow_id, run_id).await?;
    let history = store.read_history(workflow_id).await?;
    let payload =
        result.map_err(|error| format!("workflow failed: {error:?}; history: {history:#?}"))?;
    Ok(serde_json::from_slice(payload.bytes())?)
}

fn count<F>(history: &[Event], predicate: F) -> usize
where
    F: Fn(&Event) -> bool,
{
    history.iter().filter(|event| predicate(event)).count()
}

fn child_started_count(history: &[Event]) -> usize {
    count(history, |event| {
        matches!(event, Event::ChildWorkflowStarted { .. })
    })
}

fn workflow_started_count(history: &[Event]) -> usize {
    count(history, |event| {
        matches!(event, Event::WorkflowStarted { .. })
    })
}

fn activity_in_flight(history: &[Event]) -> bool {
    count(history, |event| {
        matches!(event, Event::ActivityStarted { .. })
    }) > 0
        && count(history, |event| {
            matches!(
                event,
                Event::ActivityCompleted { .. } | Event::ActivityFailed { .. }
            )
        }) == 0
}

/// Walk the chain top-down and return `(level_two_id, level_three_id)` once
/// both spawns are durably recorded.
async fn chain_ids(
    store: &Arc<dyn EventStore>,
    level_one_id: &WorkflowId,
) -> Result<(WorkflowId, WorkflowId), Box<dyn std::error::Error>> {
    let level_two_id = wait_for_child_started(store, level_one_id, "level_two").await?;
    let level_three_id = wait_for_child_started(store, &level_two_id, "level_three").await?;
    Ok((level_two_id, level_three_id))
}

fn decoded_string(payload: &Payload) -> Result<String, Box<dyn std::error::Error>> {
    let value: Value = serde_json::from_slice(payload.bytes())?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("payload was not a JSON string: {value}").into())
}

/// (a) Three-level completion: every level is a full workflow with its own
/// history, results propagate bottom-up, and each parent's
/// `ChildWorkflowStarted`/`ChildWorkflowCompleted` pair is recorded exactly
/// once with the payloads intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_level_chain_completes_and_propagates_results_bottom_up() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let dispatcher = LeafDispatcher::new(None);
    let engine = engine_over(&store, &dispatcher).await?;

    let (l1, l1_run) = start(&engine, "level_one", chain_input("job-chain", false)?).await?;
    let output = completed_result(&engine, &store, &l1, &l1_run).await?;
    assert_eq!(output, json!("l1:l2:l3:done:job-chain"));

    let h1 = store.read_history(&l1).await?;
    let l2 = child_started_id(&h1, "level_two").ok_or("level_one recorded no level_two spawn")?;
    let h2 = store.read_history(&l2).await?;
    let l3 =
        child_started_id(&h2, "level_three").ok_or("level_two recorded no level_three spawn")?;
    let h3 = store.read_history(&l3).await?;

    for (history, label) in [(&h1, "level_one"), (&h2, "level_two"), (&h3, "level_three")] {
        assert_eq!(
            status_from_events(history),
            WorkflowStatus::Completed,
            "{label} history: {history:#?}"
        );
        assert_eq!(workflow_started_count(history), 1, "{label}: {history:#?}");
    }

    // Each parent recorded exactly one spawn and one completion for its
    // child, with the propagated result decoding to the child's output.
    let expected_input: Value = json!({ "job_id": "job-chain", "note": NOTE, "gate": false });
    for (parent, child_id, child_type, child_output) in [
        (&h1, &l2, "level_two", "l2:l3:done:job-chain"),
        (&h2, &l3, "level_three", "l3:done:job-chain"),
    ] {
        assert_eq!(child_started_count(parent), 1, "{child_type}: {parent:#?}");
        let (recorded_input, recorded_type) = parent
            .iter()
            .find_map(|event| match event {
                Event::ChildWorkflowStarted {
                    child_workflow_id,
                    workflow_type,
                    input,
                    ..
                } if child_workflow_id == child_id => Some((input.clone(), workflow_type.clone())),
                _ => None,
            })
            .ok_or("parent lost its ChildWorkflowStarted")?;
        assert_eq!(recorded_type, child_type);
        assert!(
            recorded_input.bytes().len() > 64,
            "spawn inputs must stay on the large-payload path: {} bytes",
            recorded_input.bytes().len()
        );
        let recorded_value: Value = serde_json::from_slice(recorded_input.bytes())?;
        assert_eq!(recorded_value, expected_input);
        let completion = parent
            .iter()
            .find_map(|event| match event {
                Event::ChildWorkflowCompleted {
                    child_workflow_id,
                    result,
                    ..
                } if child_workflow_id == child_id => Some(result.clone()),
                _ => None,
            })
            .ok_or(format!(
                "parent recorded no ChildWorkflowCompleted for {child_type}"
            ))?;
        assert_eq!(decoded_string(&completion)?, child_output);

        // The child's own start carries the exact spawned input. Parent
        // linkage lives solely in the parent's ChildWorkflowStarted today:
        // the child's WorkflowStarted records no parent_run_id.
        let child_history = store.read_history(child_id).await?;
        let started = child_history
            .iter()
            .find_map(|event| match event {
                Event::WorkflowStarted {
                    workflow_type,
                    input,
                    parent_run_id,
                    ..
                } => Some((workflow_type.clone(), input.clone(), parent_run_id.clone())),
                _ => None,
            })
            .ok_or("child has no WorkflowStarted")?;
        assert_eq!(started.0, child_type);
        assert_eq!(started.1, recorded_input);
        assert_eq!(started.2, None, "pin: child starts record no parent_run_id");
    }

    // The leaf actually ran its activity, once, with the large payloads.
    assert_eq!(dispatcher.dispatch_count(), 1);
    assert_eq!(
        count(&h3, |event| matches!(
            event,
            Event::ActivityScheduled { activity_type, .. } if activity_type == "leaf_work"
        )),
        1,
        "level_three history: {h3:#?}"
    );
    let leaf_result = h3
        .iter()
        .find_map(|event| match event {
            Event::ActivityCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .ok_or("level_three recorded no ActivityCompleted")?;
    assert!(leaf_result.bytes().len() > 64);

    engine.shutdown()?;
    Ok(())
}

/// (b) Query at depth: with the leaf's activity gated in flight, the whole
/// chain is parked — `level_one` in `child.await`, `level_three` at the
/// activity await. Both answer queries, repeatedly, and no history at any
/// level gains a single event from the query path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queries_at_top_and_leaf_answered_while_leaf_activity_parked() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gate = Gate::new();
    let dispatcher = LeafDispatcher::new(Some(Arc::clone(&gate)));
    let engine = engine_over(&store, &dispatcher).await?;

    let (l1, l1_run) = start(&engine, "level_one", chain_input("job-query", false)?).await?;
    let (l2, l3) = chain_ids(&store, &l1).await?;
    let h3 = wait_for_history(&store, &l3, "leaf activity in flight", activity_in_flight).await?;
    let l3_run = run_id_of(&h3)?;

    let top = query_when_registered(&engine, &l1, &l1_run, "level_one_status").await?;
    assert_eq!(top, json!("awaiting-level-two:job-query"));
    let leaf = query_when_registered(&engine, &l3, &l3_run, "level_three_status").await?;
    assert_eq!(leaf, json!("processing:job-query"));

    let parked_one = store.read_history(&l1).await?;
    let parked_two = store.read_history(&l2).await?;
    let parked_three = store.read_history(&l3).await?;
    for _ in 0..3 {
        let top = query_when_registered(&engine, &l1, &l1_run, "level_one_status").await?;
        assert_eq!(top, json!("awaiting-level-two:job-query"));
        let leaf = query_when_registered(&engine, &l3, &l3_run, "level_three_status").await?;
        assert_eq!(leaf, json!("processing:job-query"));
    }
    assert_eq!(
        store.read_history(&l1).await?,
        parked_one,
        "queries must never append to level_one"
    );
    assert_eq!(
        store.read_history(&l2).await?,
        parked_two,
        "queries must never append to level_two"
    );
    assert_eq!(
        store.read_history(&l3).await?,
        parked_three,
        "queries must never append to level_three"
    );

    gate.release();
    let output = completed_result(&engine, &store, &l1, &l1_run).await?;
    assert_eq!(output, json!("l1:l2:l3:done:job-query"));

    engine.shutdown()?;
    Ok(())
}

/// (c) Recovery at depth, byte-stable park: the leaf's activity terminal is
/// recorded and `level_three` parks on its `leaf_release` signal; the
/// engine is killed and rebuilt over the same store. All three recovered
/// histories replay byte-identically, nothing is re-dispatched, no child is
/// respawned (positional correlation holds at every level), and the
/// post-recovery release signal drives the whole chain to completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_at_depth_replays_all_three_histories_byte_identical() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let first_dispatcher = LeafDispatcher::new(None);
    let first_engine = engine_over(&store, &first_dispatcher).await?;

    let (l1, l1_run) = start(
        &first_engine,
        "level_one",
        chain_input("job-recover", true)?,
    )
    .await?;
    let (l2, l3) = chain_ids(&store, &l1).await?;
    // The leaf's activity terminal is durable; the run is parked at the
    // signal receive, so histories are byte-stable across the kill.
    let pre_kill_three = wait_for_history(&store, &l3, "leaf activity terminal", |events| {
        count(events, |event| {
            matches!(event, Event::ActivityCompleted { .. })
        }) == 1
    })
    .await?;
    let l3_run = run_id_of(&pre_kill_three)?;
    let pre_kill_one = store.read_history(&l1).await?;
    let pre_kill_two = store.read_history(&l2).await?;
    assert_eq!(first_dispatcher.dispatch_count(), 1);
    first_engine.shutdown()?;

    // Rebuild over the same store: startup recovery re-spawns all three
    // live runs and replay parks each at its recorded yield point.
    let recovery_dispatcher = LeafDispatcher::new(None);
    let recovered = engine_over(&store, &recovery_dispatcher).await?;
    for (id, run, label) in [
        (&l1, &l1_run, "level_one"),
        (&l2, &run_id_of(&pre_kill_two)?, "level_two"),
        (&l3, &l3_run, "level_three"),
    ] {
        let handle = recovered.registry().get(id, run)?;
        assert!(
            handle.is_some_and(|handle| handle.cached_status() == WorkflowStatus::Running),
            "{label} must recover as a running resident process"
        );
    }

    // Replay has demonstrably reached the park points (both queries answer
    // again — handler re-registration is organic replay) and every history
    // is byte-identical: no duplicated starts, no respawned children, no
    // re-recorded events.
    let top = query_when_registered(&recovered, &l1, &l1_run, "level_one_status").await?;
    assert_eq!(top, json!("awaiting-level-two:job-recover"));
    let leaf = query_when_registered(&recovered, &l3, &l3_run, "level_three_status").await?;
    assert_eq!(leaf, json!("processing:job-recover"));
    let post_one = store.read_history(&l1).await?;
    let post_two = store.read_history(&l2).await?;
    let post_three = store.read_history(&l3).await?;
    assert_eq!(
        post_one, pre_kill_one,
        "level_one replay must be byte-identical"
    );
    assert_eq!(
        post_two, pre_kill_two,
        "level_two replay must be byte-identical"
    );
    assert_eq!(
        post_three, pre_kill_three,
        "level_three replay must be byte-identical"
    );
    for (history, label) in [(&post_one, "level_one"), (&post_two, "level_two")] {
        assert_eq!(
            child_started_count(history),
            1,
            "{label} must not respawn its child: {history:#?}"
        );
    }
    assert_eq!(workflow_started_count(&post_three), 1);
    assert_eq!(
        recovery_dispatcher.dispatch_count(),
        0,
        "a recorded activity terminal must never be re-dispatched"
    );

    // Release the recovered leaf and the chain completes bottom-up.
    recovered
        .signal(
            &l3,
            &l3_run,
            "leaf_release",
            Payload::from_json(&json!(RELEASE_TOKEN))?,
        )
        .await?;
    let output = completed_result(&recovered, &store, &l1, &l1_run).await?;
    assert_eq!(
        output,
        json!(format!("l1:l2:l3:done:job-recover:{RELEASE_TOKEN}"))
    );
    let final_three = store.read_history(&l3).await?;
    assert_eq!(
        &final_three[..pre_kill_three.len()],
        &pre_kill_three[..],
        "the settled prefix must stay byte-identical through completion"
    );

    recovered.shutdown()?;
    Ok(())
}

/// (c, mid-activity variant) Kill the engine while the leaf's `leaf_work`
/// dispatch is genuinely in flight (scheduled + started, no terminal).
///
/// CURRENT SEMANTICS, pinned: an in-flight activity is at-least-once. On
/// recovery the replay resolver finds no recorded terminal for the
/// scheduled activity (`durability/cursor.rs::resolve_activity` returns
/// `Exhausted`), resumes live, and re-dispatches — appending a fresh
/// `ActivityScheduled`/`ActivityStarted` pair for the SAME activity id
/// (`runtime/nif_activity_dispatch.rs` records before every live
/// dispatch). The parents parked in `child.await` replay byte-identically
/// and respawn nothing: positional correlation holds at every level.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_mid_leaf_activity_redispatches_without_respawning_children() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let first_gate = Gate::new();
    let first_dispatcher = LeafDispatcher::new(Some(Arc::clone(&first_gate)));
    let first_engine = engine_over(&store, &first_dispatcher).await?;

    let (l1, l1_run) = start(
        &first_engine,
        "level_one",
        chain_input("job-midact", false)?,
    )
    .await?;
    let (l2, l3) = chain_ids(&store, &l1).await?;
    let pre_kill_three =
        wait_for_history(&store, &l3, "leaf activity in flight", activity_in_flight).await?;
    let l3_run = run_id_of(&pre_kill_three)?;
    // The leaf is parked at the activity await (its query answers there).
    let leaf = query_when_registered(&first_engine, &l3, &l3_run, "level_three_status").await?;
    assert_eq!(leaf, json!("processing:job-midact"));
    let pre_kill_one = store.read_history(&l1).await?;
    let pre_kill_two = store.read_history(&l2).await?;
    assert_eq!(first_dispatcher.dispatch_count(), 1);
    // Kill with the dispatch blocked on the first gate. The gate stays held
    // until the end of the test: the dead engine's blocking task must not
    // settle while the recovered engine's assertions run.
    first_engine.shutdown()?;

    let second_gate = Gate::new();
    let second_dispatcher = LeafDispatcher::new(Some(Arc::clone(&second_gate)));
    let recovered = engine_over(&store, &second_dispatcher).await?;

    // The leaf replays to the dispatch, finds no recorded terminal, and
    // re-dispatches: exactly one fresh Scheduled/Started pair for the same
    // activity id, appended after the byte-identical pre-kill prefix.
    let redispatched = wait_for_history(&store, &l3, "leaf re-dispatch recorded", |events| {
        events.len() >= pre_kill_three.len() + 2
    })
    .await?;
    assert_eq!(
        &redispatched[..pre_kill_three.len()],
        &pre_kill_three[..],
        "the pre-kill prefix must stay byte-identical"
    );
    assert_eq!(
        redispatched.len(),
        pre_kill_three.len() + 2,
        "{redispatched:#?}"
    );
    let first_id = pre_kill_three
        .iter()
        .find_map(|event| match event {
            Event::ActivityScheduled { activity_id, .. } => Some(activity_id.clone()),
            _ => None,
        })
        .ok_or("no recorded ActivityScheduled before the kill")?;
    match (
        &redispatched[pre_kill_three.len()],
        &redispatched[pre_kill_three.len() + 1],
    ) {
        (
            Event::ActivityScheduled {
                activity_id: scheduled,
                activity_type,
                ..
            },
            Event::ActivityStarted {
                activity_id: started,
                ..
            },
        ) => {
            assert_eq!(activity_type, "leaf_work");
            assert_eq!(
                scheduled, &first_id,
                "the re-dispatch must reuse the positional activity id"
            );
            assert_eq!(started, &first_id);
        }
        other => return Err(format!("expected a re-dispatch pair, found {other:?}").into()),
    }
    assert_eq!(
        second_dispatcher.dispatch_count(),
        1,
        "exactly one re-dispatch"
    );

    // The parents replayed byte-identically and respawned nothing.
    assert_eq!(store.read_history(&l1).await?, pre_kill_one);
    assert_eq!(store.read_history(&l2).await?, pre_kill_two);
    assert_eq!(child_started_count(&store.read_history(&l1).await?), 1);
    assert_eq!(child_started_count(&store.read_history(&l2).await?), 1);
    assert_eq!(workflow_started_count(&redispatched), 1);

    // Release the recovered dispatch: the chain completes bottom-up with
    // exactly one recorded activity terminal.
    second_gate.release();
    let output = completed_result(&recovered, &store, &l1, &l1_run).await?;
    assert_eq!(output, json!("l1:l2:l3:done:job-midact"));
    let final_three = store.read_history(&l3).await?;
    assert_eq!(
        count(&final_three, |event| matches!(
            event,
            Event::ActivityCompleted { .. }
        )),
        1,
        "level_three history: {final_three:#?}"
    );

    // Unblock the dead engine's leaked dispatcher thread; its completion is
    // delivered to the shut-down runtime and dropped.
    first_gate.release();
    recovered.shutdown()?;
    Ok(())
}

/// (d) Recursion reality-check: `level_self` spawns its own workflow type
/// with `depth + 1` until `max_depth = 3`, so the chain is three runs of
/// one type. The engine is killed while the deepest run's activity is in
/// flight and rebuilt: both ancestor histories replay byte-identically with
/// no respawn, and the released chain completes depth-tagged bottom-up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn self_spawning_recursion_stops_at_depth_three_and_recovers() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let first_gate = Gate::new();
    let first_dispatcher = LeafDispatcher::new(Some(Arc::clone(&first_gate)));
    let first_engine = engine_over(&store, &first_dispatcher).await?;

    let input = Payload::from_json(
        &json!({ "depth": 1, "max_depth": 3, "job_id": "job-self", "note": NOTE }),
    )?;
    let (d1, d1_run) = start(&first_engine, "level_self", input).await?;
    let d2 = wait_for_child_started(&store, &d1, "level_self").await?;
    let d3 = wait_for_child_started(&store, &d2, "level_self").await?;
    let pre_kill_deep = wait_for_history(
        &store,
        &d3,
        "deepest activity in flight",
        activity_in_flight,
    )
    .await?;

    // The depth parameter stopped the recursion: the deepest run dispatched
    // the activity instead of spawning, and every run is the same type.
    assert_eq!(child_started_count(&pre_kill_deep), 0, "{pre_kill_deep:#?}");
    for id in [&d1, &d2, &d3] {
        let history = store.read_history(id).await?;
        let recorded_type = history
            .iter()
            .find_map(|event| match event {
                Event::WorkflowStarted { workflow_type, .. } => Some(workflow_type.clone()),
                _ => None,
            })
            .ok_or("recursive run has no WorkflowStarted")?;
        assert_eq!(recorded_type, "level_self");
    }
    let pre_kill_top = store.read_history(&d1).await?;
    let pre_kill_mid = store.read_history(&d2).await?;
    first_engine.shutdown()?;

    // Recovery at recursion depth: ancestors replay byte-identically and
    // respawn nothing; the deepest run re-dispatches its in-flight activity
    // (the at-least-once pin proven above).
    let second_gate = Gate::new();
    let second_dispatcher = LeafDispatcher::new(Some(Arc::clone(&second_gate)));
    let recovered = engine_over(&store, &second_dispatcher).await?;
    wait_for_history(&store, &d3, "deepest re-dispatch recorded", |events| {
        events.len() >= pre_kill_deep.len() + 2
    })
    .await?;
    assert_eq!(store.read_history(&d1).await?, pre_kill_top);
    assert_eq!(store.read_history(&d2).await?, pre_kill_mid);
    assert_eq!(child_started_count(&pre_kill_top), 1);
    assert_eq!(child_started_count(&pre_kill_mid), 1);

    second_gate.release();
    let output = completed_result(&recovered, &store, &d1, &d1_run).await?;
    assert_eq!(output, json!("d1<d2<d3:done:job-self"));
    for id in [&d1, &d2, &d3] {
        let history = store.read_history(id).await?;
        assert_eq!(status_from_events(&history), WorkflowStatus::Completed);
        assert_eq!(workflow_started_count(&history), 1);
    }

    first_gate.release();
    recovered.shutdown()?;
    Ok(())
}

/// (Q2) CURRENT CANCELLATION SEMANTICS, pinned — pending a real design
/// (Temporal-style parent-close policies are the likely future shape).
///
/// `Engine::cancel` (`src/lifecycle/terminate.rs`) records the target's
/// `WorkflowCancelled` and kills its process; runtime link propagation
/// tears down linked ACTIVITY processes only. Child workflows are separate
/// unlinked workflows by design (`src/child/spawn.rs`: "Children are not
/// process-linked to their parents"), so cancelling `level_two`:
///
/// - records nothing in `level_three` and does not stop it: the grandchild
///   is silently orphaned, keeps Running, and completes on its own;
/// - surfaces to `level_one`'s await as a recorded `ChildWorkflowFailed`
///   whose message is `cancelled:<reason>` (the watcher's D-4 mapping in
///   `src/runtime/nif_child_watch.rs` — no distinct cancelled taxonomy);
/// - appends nothing to the cancelled `level_two` when the orphan later
///   completes: the watcher `level_two` armed for `level_three` is aborted
///   when `level_two`'s process exits (`nif_state.rs::cleanup_process`),
///   and the record path's atomic parent-terminal guard
///   (`record_parent_child_terminal`) backstops any race.
///
/// This test pins that behavior; it does not endorse it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_level_two_orphans_level_three_and_fails_level_one_await() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gate = Gate::new();
    let dispatcher = LeafDispatcher::new(Some(Arc::clone(&gate)));
    let engine = engine_over(&store, &dispatcher).await?;

    let (l1, l1_run) = start(&engine, "level_one", chain_input("job-cancel", false)?).await?;
    let (l2, l3) = chain_ids(&store, &l1).await?;
    let h3 = wait_for_history(&store, &l3, "leaf activity in flight", activity_in_flight).await?;
    let l3_run = run_id_of(&h3)?;
    let l2_run = run_id_of(&store.read_history(&l2).await?)?;

    let reason = "operator cancelled level two";
    engine.cancel(&l2, &l2_run, reason).await?;

    // The cancelled child records its terminal and nothing after it.
    let cancelled_two = wait_for_history(&store, &l2, "level_two cancelled", |events| {
        matches!(events.last(), Some(Event::WorkflowCancelled { .. }))
    })
    .await?;
    assert_eq!(
        status_from_events(&cancelled_two),
        WorkflowStatus::Cancelled
    );
    match cancelled_two.last() {
        Some(Event::WorkflowCancelled {
            reason: recorded, ..
        }) => assert_eq!(recorded, reason),
        other => return Err(format!("expected WorkflowCancelled, found {other:?}").into()),
    }

    // The awaiting parent gets the recorded child terminal as a failure
    // with the cancelled:<reason> marker, and (by fixture design) completes
    // with the exact text its await observed.
    let parent = wait_for_history(&store, &l1, "parent-side child terminal", |events| {
        count(events, |event| {
            matches!(event, Event::ChildWorkflowFailed { .. })
        }) == 1
    })
    .await?;
    let recorded_error = parent
        .iter()
        .find_map(|event| match event {
            Event::ChildWorkflowFailed {
                child_workflow_id,
                error,
                ..
            } if child_workflow_id == &l2 => Some(error.clone()),
            _ => None,
        })
        .ok_or("level_one recorded no ChildWorkflowFailed for level_two")?;
    assert_eq!(recorded_error.message, format!("cancelled:{reason}"));
    let output = completed_result(&engine, &store, &l1, &l1_run).await?;
    assert_eq!(output, json!(format!("l1-child-failed:cancelled:{reason}")));

    // The grandchild was NOT cancelled: no propagation reaches it. It is
    // still a live registered run with an in-flight activity and no record
    // of its parent's fate.
    let orphaned = store.read_history(&l3).await?;
    assert_eq!(status_from_events(&orphaned), WorkflowStatus::Running);
    assert_eq!(
        orphaned, h3,
        "cancellation must not touch the grandchild's history"
    );
    assert!(
        engine.registry().get(&l3, &l3_run)?.is_some(),
        "the orphaned grandchild must still be registered as live"
    );

    // The orphan completes on its own, and its terminal is recorded nowhere
    // but its own history: the cancelled parent's history stays frozen.
    gate.release();
    let completed_three = wait_for_history(&store, &l3, "orphan completes", |events| {
        matches!(events.last(), Some(Event::WorkflowCompleted { .. }))
    })
    .await?;
    assert_eq!(
        status_from_events(&completed_three),
        WorkflowStatus::Completed
    );
    let orphan_result = completed_three
        .iter()
        .find_map(|event| match event {
            Event::WorkflowCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .ok_or("orphan recorded no result")?;
    assert_eq!(decoded_string(&orphan_result)?, "l3:done:job-cancel");
    // Ordering anchor for the negative assertion below: the orphan's exit
    // bookkeeping reconciles its registry projection to Completed strictly
    // AFTER the completion doorbell fires — the only trigger a leaked
    // watcher could have. (The watcher itself was aborted at level_two's
    // process exit, and the record path's parent-terminal guard makes any
    // racing append impossible; this wait just gives a regression a real
    // window to expose itself instead of a bare sleep.)
    let reconcile_deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let status = engine
            .registry()
            .get(&l3, &l3_run)?
            .map(|handle| handle.cached_status());
        if status == Some(WorkflowStatus::Completed) {
            break;
        }
        if Instant::now() >= reconcile_deadline {
            return Err(format!("orphan registry projection never reconciled: {status:?}").into());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_eq!(
        store.read_history(&l2).await?,
        cancelled_two,
        "the orphan's completion must append nothing to its cancelled parent"
    );

    engine.shutdown()?;
    Ok(())
}
