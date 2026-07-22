//! In-VM tier activity dispatch end-to-end tests over `InMemoryStore`.
//!
//! CUT 3 core invariant, proven over a real engine with NO activity
//! dispatcher configured: an `InVm` activity's runner executes ONCE, live, in
//! a linked child process; its recorded history is shape-identical to a
//! remote dispatch (`ActivityScheduled`/`ActivityStarted`/terminal with task
//! queue, node, and attempt stamped); replay resolves the recording without
//! re-executing the runner; a runner crash surfaces as a proper
//! `ActivityFailed` (workflow process survives); node death after
//! Scheduled/Started reopens and re-dispatches through the existing
//! replay-reopen path; and `with_timeout` scope expiry records the durable
//! timeout failure over a hanging runner.
//!
//! The runs-once proofs count runner executions through a host-registered
//! `invm_test_host:bump/1` NIF keyed by the workflow input, so parallel tests
//! never share a counter.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use aion::runtime::{Mfa, NifEntry};
use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{
    ActivityId, Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus,
    status_from_events,
};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore, WriteToken};
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary_ref::BinaryRef;
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FIXTURE_MODULE: &str = "aion_invm_fixture";
const FIXTURE_BEAM: &[u8] = include_bytes!("fixtures/aion_invm_fixture.beam");
const FIXTURE_SOURCE: &[u8] = include_bytes!("fixtures/aion_invm_fixture.erl");

/// Per-key runner-execution counters behind the `invm_test_host:bump/1` NIF.
/// Keys are the raw workflow-input JSON bytes each fixture thunk captures.
static COUNTERS: LazyLock<Mutex<HashMap<Vec<u8>, i64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Host NIF `invm_test_host:bump/1`: increment and return the counter for the
/// binary key argument.
fn bump_nif(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let _ = ctx;
    if args.len() != 1 {
        return Err(Term::NIL);
    }
    let Some(key) = BinaryRef::new(args[0]) else {
        return Err(Term::NIL);
    };
    let key = key.as_bytes().to_vec();
    let Ok(mut counters) = COUNTERS.lock() else {
        return Err(Term::NIL);
    };
    let count = counters.entry(key).or_insert(0);
    *count += 1;
    Ok(Term::small_int(*count))
}

fn counter_for(key: &str) -> i64 {
    COUNTERS
        .lock()
        .map(|counters| {
            counters
                .get(format!("\"{key}\"").as_bytes())
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(-1)
}

fn unique_key(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

fn fixture_package(entry_function: &str) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(FIXTURE_MODULE, FIXTURE_BEAM)])?;
    let manifest = Manifest {
        entry_module: FIXTURE_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "string" }),
        output_schema: json!({}),
        timeout: Some(Duration::from_secs(30)),
        activities: vec![DeclaredActivity {
            activity_type: "invm_work".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(FIXTURE_MODULE, FIXTURE_SOURCE.to_vec())])
            .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

/// Build an engine over `store` with the fixture loaded and the counter NIF
/// registered — and deliberately NO activity dispatcher: the in-VM tier must
/// work without one.
async fn engine_over(
    store: &Arc<dyn EventStore>,
    entry_function: &str,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .register_nifs([NifEntry::new(
            Mfa::new("invm_test_host", "bump", 1),
            bump_nif,
        )])
        .load_workflows(fixture_package(entry_function)?)
        .build()
        .await?)
}

async fn wait_for_event(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    matches: impl Fn(&Event) -> bool,
    what: &str,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let history = store.read_history(workflow_id).await?;
        if history.iter().any(&matches) {
            return Ok(history);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}: {history:#?}").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn start(
    engine: &Engine,
    key: &str,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            FIXTURE_MODULE,
            Payload::from_json(&json!(key))?,
            HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

async fn release(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<(), Box<dyn std::error::Error>> {
    engine
        .signal(
            workflow_id,
            run_id,
            "release",
            Payload::from_json(&json!({}))?,
        )
        .await?;
    Ok(())
}

/// The core invariant end-to-end: the runner executes once live, the recorded
/// history is remote-shaped, and a fresh engine epoch over the same store
/// replays the recording while appending nothing and re-running nothing.
#[tokio::test(flavor = "multi_thread")]
async fn in_vm_activity_runs_once_and_replay_returns_the_recording() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "run_once_gated").await?;
    let key = unique_key("invm-once");
    let (workflow_id, run_id) = start(&engine, &key).await?;

    let history = wait_for_event(
        &store,
        &workflow_id,
        |event| matches!(event, Event::ActivityCompleted { .. }),
        "the in-VM activity completion",
    )
    .await?;
    assert_eq!(counter_for(&key), 1, "the runner must have executed once");

    // Remote-shaped history: the same Scheduled/Started/Completed the remote
    // wire records, with task queue, node, and attempt stamped identically.
    let scheduled = history
        .iter()
        .find_map(|event| match event {
            Event::ActivityScheduled {
                activity_id,
                activity_type,
                task_queue,
                node,
                ..
            } => Some((
                activity_id.clone(),
                activity_type.clone(),
                task_queue.clone(),
                node.clone(),
            )),
            _ => None,
        })
        .ok_or("missing ActivityScheduled")?;
    assert_eq!(scheduled.0, ActivityId::from_sequence_position(0));
    assert_eq!(scheduled.1, "invm_work");
    assert_eq!(scheduled.2, "default");
    assert_eq!(scheduled.3, None);
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::ActivityStarted { attempt: 1, .. }))
    );
    let completed = history
        .iter()
        .find_map(|event| match event {
            Event::ActivityCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .ok_or("missing ActivityCompleted")?;
    assert_eq!(completed.bytes(), b"1", "the recorded result is the count");

    // Crash analogue: stop the engine with the terminal recorded but the run
    // still live (parked on the release signal), then replay a fresh epoch.
    let pre_restart = store.read_history(&workflow_id).await?;
    engine.shutdown()?;
    let recovered = engine_over(&store, "run_once_gated").await?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while recovered.registry().get(&workflow_id, &run_id)?.is_none() {
        assert!(
            Instant::now() < deadline,
            "recovered workflow was not re-registered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Replay resolved the recorded dispatch + completion: nothing appended,
    // and the runner did NOT re-execute.
    assert_eq!(store.read_history(&workflow_id).await?, pre_restart);
    assert_eq!(counter_for(&key), 1, "replay must not re-run the runner");

    release(&recovered, &workflow_id, &run_id).await?;
    let result = recovered.result(&workflow_id, &run_id).await?;
    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    assert_eq!(payload.bytes(), b"1");
    assert_eq!(counter_for(&key), 1);
    recovered.shutdown()?;
    Ok(())
}

/// A runner returning `Error(Retryable)` records `ActivityFailed` once and
/// the workflow observes the retryable kind end-to-end (prefix fidelity
/// across the child boundary).
#[tokio::test(flavor = "multi_thread")]
async fn runner_error_kind_crosses_the_child_boundary() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "fail_retryable").await?;
    let key = unique_key("invm-fail");
    let (workflow_id, run_id) = start(&engine, &key).await?;

    let result = engine.result(&workflow_id, &run_id).await?;
    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    assert_eq!(payload.bytes(), br#""error:retryable:boom""#);
    assert_eq!(counter_for(&key), 1);

    let history = store.read_history(&workflow_id).await?;
    let failure = history
        .iter()
        .find_map(|event| match event {
            Event::ActivityFailed { error, attempt, .. } => Some((error.clone(), *attempt)),
            _ => None,
        })
        .ok_or("missing ActivityFailed")?;
    assert_eq!(failure.0.message, "retryable:boom");
    assert_eq!(failure.1, 1);
    engine.shutdown()?;
    Ok(())
}

/// A crashing runner (badmatch in the thunk) kills only the CHILD: the
/// abnormal exit is synthesized into a terminal `ActivityFailed` and the
/// workflow process survives to observe it as data and complete normally.
#[tokio::test(flavor = "multi_thread")]
async fn runner_crash_records_terminal_failure_and_workflow_survives() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "crash").await?;
    let key = unique_key("invm-crash");
    let (workflow_id, run_id) = start(&engine, &key).await?;

    let result = engine.result(&workflow_id, &run_id).await?;
    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    let text = String::from_utf8(payload.bytes().to_vec())?;
    assert!(
        text.starts_with(r#""error:terminal:activity process"#) && text.contains("exited"),
        "unexpected crash surface: {text}"
    );
    assert_eq!(counter_for(&key), 1);

    let history = store.read_history(&workflow_id).await?;
    assert!(
        history.iter().any(|event| matches!(
            event,
            Event::ActivityFailed { error, .. }
                if error.message.starts_with("terminal:activity process")
        )),
        "crash must be recorded as a terminal ActivityFailed: {history:#?}"
    );
    assert_eq!(status_from_events(&history), WorkflowStatus::Completed);
    engine.shutdown()?;
    Ok(())
}

/// Node death mid-activity: Scheduled+Started recorded, no terminal. A fresh
/// engine epoch replays to `ResumeLive` (the cursor walk exhausts), records the
/// reopen `ActivityScheduled` for the SAME ordinal, and re-spawns the thunk
/// the SDK re-supplies — at-least-once, no new recovery machinery.
#[tokio::test(flavor = "multi_thread")]
async fn node_death_reopens_and_redispatches_the_in_vm_thunk() -> TestResult {
    let package = fixture_package("run_once_gated")?;
    let hash = package.content_hash().to_string();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let key = unique_key("invm-reopen");
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let envelope = |seq: u64| EventEnvelope {
        seq,
        recorded_at: chrono::Utc::now(),
        workflow_id: workflow_id.clone(),
    };
    // The state a dead node leaves behind: dispatched, started, no terminal.
    let events = vec![
        Event::WorkflowStarted {
            envelope: envelope(1),
            workflow_type: FIXTURE_MODULE.to_owned(),
            input: Payload::from_json(&json!(key))?,
            run_id: run_id.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new(hash),
        },
        Event::ActivityScheduled {
            envelope: envelope(2),
            activity_id: ActivityId::from_sequence_position(0),
            activity_type: String::from("invm_work"),
            input: Payload::from_json(&json!(key))?,
            task_queue: String::from("default"),
            node: None,
        },
        Event::ActivityStarted {
            envelope: envelope(3),
            activity_id: ActivityId::from_sequence_position(0),
            attempt: 1,
        },
    ];
    store
        .append(WriteToken::recorder(), &workflow_id, &events, 0)
        .await?;

    let engine = engine_over(&store, "run_once_gated").await?;
    let history = wait_for_event(
        &store,
        &workflow_id,
        |event| matches!(event, Event::ActivityCompleted { .. }),
        "the reopened in-VM activity completion",
    )
    .await?;
    // The reopen recorded a FRESH ActivityScheduled for the same ordinal and
    // exactly one terminal; the runner executed exactly once.
    assert_eq!(
        history
            .iter()
            .filter(|event| matches!(event, Event::ActivityScheduled { .. }))
            .count(),
        2,
        "reopen must record a fresh ActivityScheduled: {history:#?}"
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| matches!(event, Event::ActivityCompleted { .. }))
            .count(),
        1
    );
    assert_eq!(counter_for(&key), 1);

    release(&engine, &workflow_id, &run_id).await?;
    let result = engine.result(&workflow_id, &run_id).await?;
    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    assert_eq!(payload.bytes(), b"1");
    engine.shutdown()?;
    Ok(())
}

/// `with_timeout` over a hanging in-VM runner: the scope expiry aborts the
/// await and records the durable timeout failure; a fresh engine epoch
/// replays it verbatim without re-running anything.
#[tokio::test(flavor = "multi_thread")]
async fn with_timeout_expiry_settles_a_hanging_in_vm_runner_durably() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "hang_with_timeout").await?;
    let key = unique_key("invm-hang");
    let (workflow_id, run_id) = start(&engine, &key).await?;

    wait_for_event(
        &store,
        &workflow_id,
        |event| {
            matches!(
                event,
                Event::ActivityFailed { error, .. } if error.message == "timeout:deadline expired"
            )
        },
        "the durable timeout failure",
    )
    .await?;
    assert_eq!(counter_for(&key), 1, "the hanging runner started once");

    let pre_restart = store.read_history(&workflow_id).await?;
    engine.shutdown()?;
    let recovered = engine_over(&store, "hang_with_timeout").await?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while recovered.registry().get(&workflow_id, &run_id)?.is_none() {
        assert!(
            Instant::now() < deadline,
            "recovered workflow was not re-registered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(store.read_history(&workflow_id).await?, pre_restart);
    assert_eq!(counter_for(&key), 1, "replay must not re-spawn the runner");

    release(&recovered, &workflow_id, &run_id).await?;
    let result = recovered.result(&workflow_id, &run_id).await?;
    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    assert_eq!(payload.bytes(), br#""timed_out""#);
    recovered.shutdown()?;
    Ok(())
}

/// Defenses: a non-closure thunk on the arity-4 wire and an in-VM tier on the
/// arity-3 remote wire are both refused with a workflow-visible error and
/// NOTHING recorded (no Activity events at all).
#[tokio::test(flavor = "multi_thread")]
async fn malformed_wires_are_refused_before_anything_is_recorded() -> TestResult {
    for (entry, expected_fragment) in [
        ("bad_thunk", "thunk argument is not a closure"),
        (
            "remote_tier_defense",
            "cannot cross the remote dispatch wire",
        ),
    ] {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = engine_over(&store, entry).await?;
        let key = unique_key("invm-defense");
        let (workflow_id, run_id) = start(&engine, &key).await?;

        let result = engine.result(&workflow_id, &run_id).await?;
        let payload = result.map_err(|error| format!("{entry} workflow failed: {error:?}"))?;
        let text = String::from_utf8(payload.bytes().to_vec())?;
        assert!(
            text.contains(expected_fragment),
            "{entry}: unexpected refusal surface: {text}"
        );
        let history = store.read_history(&workflow_id).await?;
        assert!(
            !history.iter().any(|event| matches!(
                event,
                Event::ActivityScheduled { .. }
                    | Event::ActivityStarted { .. }
                    | Event::ActivityCompleted { .. }
                    | Event::ActivityFailed { .. }
            )),
            "{entry}: a refused wire must record nothing: {history:#?}"
        );
        engine.shutdown()?;
    }
    Ok(())
}

/// Workflow death mid-activity: cancelling the workflow kills the linked
/// child through the BEAM link (side effect torn down), and no completion
/// entry stays retained once the process monitor drains (D5).
#[tokio::test(flavor = "multi_thread")]
async fn cancel_mid_activity_tears_down_the_child_and_retains_nothing() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "hang").await?;
    let key = unique_key("invm-cancel");
    let (workflow_id, run_id) = start(&engine, &key).await?;

    // The child is live: Started is recorded and the runner has begun.
    wait_for_event(
        &store,
        &workflow_id,
        |event| matches!(event, Event::ActivityStarted { .. }),
        "the in-VM activity start",
    )
    .await?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while counter_for(&key) < 1 {
        assert!(Instant::now() < deadline, "the runner never began");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    engine
        .cancel(&workflow_id, &run_id, "invm cancel test")
        .await?;
    let history = wait_for_event(
        &store,
        &workflow_id,
        |event| matches!(event, Event::WorkflowCancelled { .. }),
        "the workflow cancellation terminal",
    )
    .await?;
    assert_eq!(status_from_events(&history), WorkflowStatus::Cancelled);

    // The link killed the child; the watcher's delivery to the dead workflow
    // is refused (nothing inserted) and the monitor drain leaves the retained
    // completion maps empty.
    let deadline = Instant::now() + Duration::from_secs(20);
    while engine.runtime().retained_activity_completions() != 0 {
        assert!(
            Instant::now() < deadline,
            "retained completions were not drained: {}",
            engine.runtime().retained_activity_completions()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(counter_for(&key), 1, "the runner ran exactly once");
    engine.shutdown()?;
    Ok(())
}
