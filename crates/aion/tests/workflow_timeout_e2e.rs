//! End-to-end gates for the declared-workflow-timeout deadline (#42 / #45).
//!
//! Proves the two operator's laws and the arm→fire→TimedOut path through the
//! real engine:
//!
//! * LAW 1 — a workflow with no declared timeout arms nothing (no deadline
//!   `TimerStarted`, no `WorkflowTimedOut`).
//! * LAW 2 — a legacy/defaulted manifest (a `timeout` value NOT bound into the
//!   package's content-hash identity) can never arm, even end-to-end past the
//!   manifest's timeout.
//! * A package whose identity DOES commit to a declared timeout arms a deadline
//!   that fires to a `TimedOut` terminal with the `"workflow"` descriptor.
//! * A deadline armed on one engine fires correctly after a survivor ADOPTS the
//!   workflow's shard live (the production failover entry point).
//!
//! The timeout-bearing package is assembled by re-stamping the `sleep_query`
//! fixture's beams with a short manifest timeout and `with_explicit_timeout_identity`,
//! because a Gleam `workflow.toml` package does NOT opt into the timeout-bearing
//! identity (the D1 conservative advisory), so it reads as not-declared.

#[path = "common/example_build.rs"]
mod example_build;

use std::sync::Arc;
use std::time::{Duration, Instant};

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::{Engine, EngineBuilder};
use aion_core::{Event, Payload, RunId, WorkflowId, WorkflowStatus};
use aion_package::{ExtractionLimits, Manifest, Package, PackageBuilder};
use aion_store::{EventStore, InMemoryStore};
use aion_store_haematite::HaematiteStore;
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SHARD_COUNT: usize = 4;
const COMPLETE_DEADLINE: Duration = Duration::from_secs(30);
/// Far enough out that the workflow never completes on its own within the test:
/// the deadline is the only terminal path.
const LONG_SLEEP_MS: u64 = 600_000;

fn base_sleep_query() -> Result<Package, Box<dyn std::error::Error>> {
    example_build::built_package("crates/aion/tests/fixtures/sleep_query", "sleep_query")
}

/// Re-stamp the `sleep_query` beams with a manifest `timeout`, optionally
/// binding it into the content-hash identity.
///
/// `explicit_identity == true` yields a package whose `has_declared_timeout()`
/// is true (the deadline arms); `false` yields a legacy/defaulted-shaped
/// manifest carrying a `timeout` value that is NOT in the identity, so it reads
/// as not-declared (LAW 2).
fn sleep_query_with_timeout(
    timeout: Duration,
    explicit_identity: bool,
) -> Result<Package, Box<dyn std::error::Error>> {
    let base = base_sleep_query()?;
    let manifest = Manifest {
        timeout: Some(timeout),
        ..base.manifest().clone()
    };
    let mut builder = PackageBuilder::new(manifest, base.beams().clone());
    if explicit_identity {
        builder = builder.with_explicit_timeout_identity();
    }
    let bytes = builder.write_to_bytes()?;
    Ok(Package::load_from_bytes(
        bytes,
        ExtractionLimits::unbounded(),
    )?)
}

async fn build_engine(
    store: &Arc<dyn EventStore>,
    package: Package,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?)
}

async fn build_engine_shards(
    store: &Arc<dyn EventStore>,
    package: Package,
    owned_shards: Vec<usize>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .owned_shards(owned_shards)
        .load_workflows(package)
        .build()
        .await?)
}

async fn start_sleeper(
    engine: &Engine,
    sleep_ms: u64,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let input = Payload::from_json(&json!({ "sleep_ms": sleep_ms }))?;
    let handle = engine
        .start_workflow(
            "sleep_query",
            input,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

fn is_deadline_started(event: &Event) -> bool {
    matches!(
        event,
        Event::TimerStarted { timer_id, .. }
            if timer_id.name().is_some_and(|name| name.starts_with("deadline:"))
    )
}

fn count_timed_out(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::WorkflowTimedOut { .. }))
        .count()
}

/// Whether the store holds a durable DEADLINE timer row for the workflow,
/// inspected via `expired_timers` at a timestamp far past every plausible
/// `fire_at`. A workflow's own author sleep timers create rows too, so LAW
/// assertions must scope to the reserved `deadline:` family rather than any row.
async fn workflow_has_deadline_row(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
) -> Result<bool, Box<dyn std::error::Error>> {
    let far_future = chrono::Utc::now() + chrono::Duration::days(365 * 100);
    Ok(store
        .expired_timers(far_future)
        .await?
        .into_iter()
        .any(|entry| {
            &entry.workflow_id == workflow_id
                && entry
                    .timer_id
                    .name()
                    .is_some_and(|name| name.starts_with("deadline:"))
        }))
}

/// Spin until the run's history records its deadline `TimerStarted` but no
/// terminal yet — the deadline is armed and the run is still live.
async fn wait_until_deadline_armed(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + COMPLETE_DEADLINE;
    loop {
        let history = store.read_history(workflow_id).await?;
        let armed = history.iter().any(is_deadline_started);
        let timed_out = count_timed_out(&history) > 0;
        if armed && !timed_out {
            return Ok(history);
        }
        if Instant::now() >= deadline {
            return Err(format!("deadline never armed on the run: {history:#?}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// --- LAW 1: no declared timeout arms nothing (a hello-world with no timers). ---

struct GreetDispatcher;

impl ActivityDispatcher for GreetDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        if request.name.as_str() != "greet" {
            return Err(format!("terminal:unknown activity {}", request.name));
        }
        let value: serde_json::Value = serde_json::from_str(request.input.as_str())
            .map_err(|error| format!("terminal:bad input: {error}"))?;
        let who = value["name"].as_str().unwrap_or("stranger");
        Ok(json!({ "greeting": format!("Hello, {who}! Welcome to Aion.") }).to_string())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_declared_timeout_arms_nothing() -> TestResult {
    // hello-world has no authored timeout AND no timers, so LAW 1 is literal:
    // the run's whole history contains zero TimerStarted and no WorkflowTimedOut.
    let package = example_build::built_package("examples/hello-world", "hello_world")?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(GreetDispatcher))
        .load_workflows(package)
        .build()
        .await?;

    let handle = engine
        .start_workflow(
            "hello_world",
            Payload::from_json(&json!({ "name": "Ada" }))?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();

    let result = tokio::time::timeout(COMPLETE_DEADLINE, engine.result(&workflow_id, &run_id))
        .await
        .map_err(|_| "hello-world never completed")??;
    result.map_err(|error| format!("hello-world failed: {error:?}"))?;

    let history = store.read_history(&workflow_id).await?;
    assert!(
        !history
            .iter()
            .any(|event| matches!(event, Event::TimerStarted { .. })),
        "LAW 1: a run with no declared timeout records NO TimerStarted: {history:#?}"
    );
    assert_eq!(
        count_timed_out(&history),
        0,
        "LAW 1: no WorkflowTimedOut without a declared timeout"
    );
    // LAW 1 at the store boundary: no durable timer ROW exists for the run
    // either — not just no history event. This catches a "row scheduled but
    // TimerStarted omitted" regression the history-only assertion cannot see.
    assert!(
        !workflow_has_deadline_row(&store, &workflow_id).await?,
        "LAW 1: a run with no declared timeout has no durable timer row"
    );
    engine.shutdown()?;
    Ok(())
}

// --- LAW 2: a legacy/defaulted manifest can never arm. ---

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_defaulted_manifest_never_arms() -> TestResult {
    // The manifest carries a 300ms timeout, but it is NOT bound into the
    // package identity (no `with_explicit_timeout_identity`), exactly like a
    // legacy archive written with the old defaulted 1h value. The run must
    // outlive that timeout without arming a deadline or timing out.
    let package = sleep_query_with_timeout(Duration::from_millis(300), false)?;
    assert!(
        !package.has_declared_timeout(),
        "a legacy/defaulted manifest must read as not-declared"
    );
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = build_engine(&store, package).await?;
    let (workflow_id, run_id) = start_sleeper(&engine, 2_000).await?;

    // Live well past the manifest's 300ms timeout.
    tokio::time::sleep(Duration::from_millis(900)).await;

    let history = store.read_history(&workflow_id).await?;
    assert!(
        !history.iter().any(is_deadline_started),
        "LAW 2: a legacy/defaulted manifest arms no deadline: {history:#?}"
    );
    assert_eq!(
        count_timed_out(&history),
        0,
        "LAW 2: a legacy/defaulted manifest never times out"
    );
    assert_ne!(
        aion_core::status_from_events(&history),
        WorkflowStatus::TimedOut,
        "LAW 2: status is not TimedOut past the manifest timeout"
    );
    // LAW 2 at the store boundary: no durable deadline timer row was scheduled
    // for the legacy/defaulted manifest either.
    assert!(
        !workflow_has_deadline_row(&store, &workflow_id).await?,
        "LAW 2: a legacy/defaulted manifest schedules no durable timer row"
    );
    let _ = run_id;
    engine.shutdown()?;
    Ok(())
}

// --- Declared timeout fires to a TimedOut terminal. ---

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declared_timeout_fires_to_timed_out() -> TestResult {
    let package = sleep_query_with_timeout(Duration::from_millis(400), true)?;
    assert!(
        package.has_declared_timeout(),
        "the timeout-bearing identity must read as declared"
    );
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = build_engine(&store, package).await?;
    // The sleep is far longer than the deadline: only the deadline can end it.
    let (workflow_id, run_id) = start_sleeper(&engine, LONG_SLEEP_MS).await?;

    let result = tokio::time::timeout(COMPLETE_DEADLINE, engine.result(&workflow_id, &run_id))
        .await
        .map_err(|_| "declared-timeout workflow never reached a terminal")??;
    match result {
        Err(error) => assert!(
            error.message.contains("workflow timed out: workflow"),
            "unexpected terminal error message: {error:?}"
        ),
        Ok(payload) => return Err(format!("expected a timed-out error, got {payload:?}").into()),
    }

    let history = store.read_history(&workflow_id).await?;
    assert!(
        history.iter().any(is_deadline_started),
        "a declared timeout arms a deadline TimerStarted: {history:#?}"
    );
    assert_eq!(
        count_timed_out(&history),
        1,
        "exactly one WorkflowTimedOut terminal: {history:#?}"
    );
    match history
        .iter()
        .find(|event| matches!(event, Event::WorkflowTimedOut { .. }))
    {
        Some(Event::WorkflowTimedOut { timeout, .. }) => assert_eq!(timeout, "workflow"),
        _ => return Err("no WorkflowTimedOut event found".into()),
    }
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::TimedOut
    );
    // Late-result refusal: the workflow was parked on its long sleep when the
    // deadline fired. Teardown retires that sleep, and the terminal history
    // refuses any late append — so NO `TimerFired` (and no second terminal)
    // appears after the `WorkflowTimedOut`.
    let terminal_index = history
        .iter()
        .position(|event| matches!(event, Event::WorkflowTimedOut { .. }))
        .ok_or("no WorkflowTimedOut in history")?;
    assert!(
        !history[terminal_index + 1..]
            .iter()
            .any(|event| matches!(event, Event::TimerFired { .. })),
        "no TimerFired is appended after the timeout terminal: {history:#?}"
    );
    let _ = run_id;
    engine.shutdown()?;
    Ok(())
}

// --- Continue-as-new durably cancels the predecessor deadline (D5). ---

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn continue_as_new_cancels_the_predecessor_deadline() -> TestResult {
    // A comfortably long deadline so neither run times out during the test; we
    // only inspect the recorded transition.
    let package = sleep_query_with_timeout(Duration::from_secs(120), true)?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = build_engine(&store, package).await?;
    let (workflow_id, predecessor_run) = start_sleeper(&engine, LONG_SLEEP_MS).await?;
    wait_until_deadline_armed(&store, &workflow_id).await?;

    let successor = engine
        .continue_as_new(
            &workflow_id,
            &predecessor_run,
            Payload::from_json(&json!({ "sleep_ms": LONG_SLEEP_MS }))?,
            None,
        )
        .await?;
    let successor_run = successor.run_id().clone();

    let history = store.read_history(&workflow_id).await?;
    let predecessor_deadline = aion::time::deadline_timer_id(&predecessor_run)?;
    let successor_deadline = aion::time::deadline_timer_id(&successor_run)?;

    // The predecessor deadline is durably cancelled with WorkflowIntent, AT the
    // transition (before the successor's own WorkflowStarted).
    let cancel_index = history
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::TimerCancelled { timer_id, cause: aion_core::TimerCancelCause::WorkflowIntent, .. }
                    if timer_id == &predecessor_deadline
            )
        })
        .ok_or("predecessor deadline was not cancelled at continue-as-new")?;
    let continued_index = history
        .iter()
        .position(|event| matches!(event, Event::WorkflowContinuedAsNew { .. }))
        .ok_or("no WorkflowContinuedAsNew")?;
    assert!(
        cancel_index > continued_index,
        "the deadline cancel follows the terminal append: {history:#?}"
    );

    // The successor arms its OWN fresh deadline and it is still live.
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::TimerStarted { timer_id, .. } if timer_id == &successor_deadline)),
        "the successor arms its own deadline: {history:#?}"
    );
    assert_eq!(
        aion::time::outstanding_deadline_timer(&history, &predecessor_run),
        None,
        "the predecessor deadline is retired, so failover re-arm cannot resurrect it"
    );
    assert_eq!(
        aion::time::outstanding_deadline_timer(&history, &successor_run),
        Some(successor_deadline),
        "the successor deadline stays live"
    );
    engine.shutdown()?;
    Ok(())
}

// --- Live ops-console stream delivers the engine-fired timeout terminal. ---

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timed_out_terminal_is_delivered_on_the_live_event_stream() -> TestResult {
    use futures::StreamExt;

    let package = sleep_query_with_timeout(Duration::from_millis(400), true)?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .event_streaming(std::num::NonZeroUsize::new(256).ok_or("capacity")?)
        .load_workflows(package)
        .build()
        .await?;
    let (workflow_id, run_id) = start_sleeper(&engine, LONG_SLEEP_MS).await?;

    // A real filtered subscription scoped to this workflow.
    let mut stream = engine.subscribe(aion::EventFilter {
        workflow_id: Some(workflow_id.clone()),
        run: None,
        family: None,
    });

    // Consume the live tail until the engine-fired timeout terminal is delivered.
    let overall_deadline = Instant::now() + COMPLETE_DEADLINE;
    let mut delivered = None;
    while Instant::now() < overall_deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if matches!(event, Event::WorkflowTimedOut { .. }) {
                    delivered = Some(event);
                    break;
                }
            }
            Ok(Some(Err(_))) | Err(_) => {}
            Ok(None) => break,
        }
    }
    // The ops-console selector projects a delivered `WorkflowTimedOut` frame as
    // `TimedOut` (mapping unit-tested in aion-server's selector); here we prove
    // the engine actually publishes it live to a subscriber.
    match delivered {
        Some(Event::WorkflowTimedOut { timeout, .. }) => assert_eq!(timeout, "workflow"),
        other => {
            return Err(format!(
                "no WorkflowTimedOut frame delivered on the live stream: {other:?}"
            )
            .into());
        }
    }
    let _ = run_id;
    engine.shutdown()?;
    Ok(())
}

// --- Failover: an armed deadline fires after the survivor adopts the shard. ---

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adopted_deadline_fires_after_shard_adoption() -> TestResult {
    let dir = tempfile::tempdir()?;
    let concrete = Arc::new(HaematiteStore::create_with_shard_count(
        dir.path().join("db"),
        SHARD_COUNT,
    )?);
    let store: Arc<dyn EventStore> = Arc::clone(&concrete) as Arc<dyn EventStore>;

    // Epoch A: own all shards, start the sleeper so its deadline arms, then
    // "crash" while the deadline is still outstanding (no terminal yet).
    let engine_a = build_engine_shards(
        &store,
        sleep_query_with_timeout(Duration::from_millis(1_200), true)?,
        (0..SHARD_COUNT).collect(),
    )
    .await?;
    let (workflow_id, run_id) = start_sleeper(&engine_a, LONG_SLEEP_MS).await?;
    let armed = wait_until_deadline_armed(&store, &workflow_id).await?;
    let sleeper_shard = concrete.shard_for_workflow(&workflow_id);
    engine_a.shutdown()?;
    assert_eq!(
        count_timed_out(&armed),
        0,
        "the deadline must be armed but not fired before the failover: {armed:#?}"
    );

    // Epoch B: the survivor boots owning every shard EXCEPT the sleeper's, so
    // boot recovery never re-arms the deadline; then it ADOPTS the shard live.
    let survivor_shards: Vec<usize> = (0..SHARD_COUNT).filter(|&s| s != sleeper_shard).collect();
    let engine_b = build_engine_shards(
        &store,
        sleep_query_with_timeout(Duration::from_millis(1_200), true)?,
        survivor_shards,
    )
    .await?;
    assert!(
        engine_b.registry().get(&workflow_id, &run_id)?.is_none(),
        "the parked workflow is out of the survivor's boot scope"
    );

    engine_b.adopt_shards(&[sleeper_shard]).await?;

    let result = tokio::time::timeout(COMPLETE_DEADLINE, engine_b.result(&workflow_id, &run_id))
        .await
        .map_err(|_| "adopted deadline never fired: it was not re-armed on adoption")??;
    match result {
        Err(error) => assert!(
            error.message.contains("workflow timed out: workflow"),
            "unexpected terminal error message: {error:?}"
        ),
        Ok(payload) => return Err(format!("expected a timed-out error, got {payload:?}").into()),
    }

    let final_history = store.read_history(&workflow_id).await?;
    assert_eq!(
        count_timed_out(&final_history),
        1,
        "the adopted deadline fires exactly once: {final_history:#?}"
    );
    assert_eq!(
        aion_core::status_from_events(&final_history),
        WorkflowStatus::TimedOut
    );
    assert_eq!(
        &final_history[..armed.len()],
        armed.as_slice(),
        "the adopted resume must extend, never rewrite, the recorded history"
    );
    engine_b.shutdown()?;
    Ok(())
}
