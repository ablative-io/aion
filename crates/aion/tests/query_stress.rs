//! Stress regression for the #45 query race (F8): a failing handler must
//! always surface `HandlerFailed`, and concurrent query bursts must never
//! turn a live, answerable query into `ReplyDropped`.
//!
//! The original defect: `reply_query`/`reply_query_error` ran as dirty NIFs,
//! so every reply rode beamr's dirty-result resume, which deep-copies the
//! result onto the workflow heap without being able to GC — a reply landing
//! on a full heap killed the workflow process with `Badarg`, exit-time
//! cleanup dropped the parked reply senders, and the caller observed
//! `ReplyDropped` where `HandlerFailed` (or a payload) was correct. Before
//! the fix this loop failed within the first handful of iterations on every
//! run; the original review reproduced it at roughly 1/170 under full-suite
//! load.

use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, EngineError, QueryError, RuntimeHandle, SignalRouter};
use aion_core::{Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

const QUERY_MODULE: &str = "aion_fixture_query";
const QUERY_BEAM: &[u8] = include_bytes!("fixtures/aion_fixture_query.beam");
const QUERY_SOURCE: &[u8] = include_bytes!("fixtures/aion_fixture_query.erl");

/// Generous engine reply deadline for tests where queries must succeed.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for fixture handler registration (workflow code races the caller).
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(20);
/// Failing-handler iterations; the pre-fix defect fired within the first few.
const BOOM_ITERATIONS: u32 = 400;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn query_package(entry_function: &str) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(QUERY_MODULE, QUERY_BEAM)])?;
    let manifest = Manifest {
        entry_module: QUERY_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(QUERY_MODULE, QUERY_SOURCE.to_vec())])
            .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

async fn engine_over(store: &Arc<dyn EventStore>) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .query_timeout(QUERY_TIMEOUT)
        .load_workflows(query_package("queryable")?)
        .build()
        .await?)
}

fn fixture_input() -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "fixture": "input" }))
}

async fn start(engine: &Engine) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            QUERY_MODULE,
            fixture_input()?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

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

#[tokio::test(flavor = "multi_thread")]
async fn failing_handler_is_always_handler_failed_under_stress() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = Arc::new(engine_over(&store).await?);
    let (workflow_id, run_id) = start(&engine).await?;
    // Warm up: the fixture finished registering its handlers.
    query_when_registered(&engine, &workflow_id, &run_id, "state").await?;

    // Background contention: three more parked workflows answering bursts of
    // state queries while the foreground loop hammers the failing handler.
    let mut background = Vec::new();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for _ in 0..3 {
        let (noise_id, noise_run) = start(&engine).await?;
        query_when_registered(&engine, &noise_id, &noise_run, "state").await?;
        let engine = Arc::clone(&engine);
        let stop = Arc::clone(&stop);
        background.push(tokio::spawn(async move {
            let mut failures = Vec::new();
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let burst = futures::future::join_all(
                    (0..4).map(|_| engine.query(&noise_id, &noise_run, "state")),
                )
                .await;
                for outcome in burst {
                    match outcome {
                        // A timeout is tolerated alongside success: the beamr
                        // 0.4.9 lost-wakeup window (see the foreground loop);
                        // the next burst's deliveries self-heal it.
                        Ok(_) | Err(EngineError::Query(QueryError::Timeout)) => {}
                        Err(error) => failures.push(format!("{error:?}")),
                    }
                }
                // Bounded contention: unthrottled bursts from three callers
                // saturate the single-thread scheduler into multi-second
                // service starvation, which is a load pathology rather than
                // the reply race this test pins.
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            failures
        }));
    }

    let mut wrong = Vec::new();
    let mut lost_wakeup_timeouts = 0_u32;
    for iteration in 0..BOOM_ITERATIONS {
        match engine.query(&workflow_id, &run_id, "boom").await {
            Err(EngineError::Query(QueryError::HandlerFailed { message })) => {
                assert!(
                    message.contains("fixture boom"),
                    "iteration {iteration}: failure must carry the raise reason: {message}"
                );
            }
            // Known upstream defect, NOT the race this test pins: beamr
            // 0.4.9 has a lost-wakeup window in the scheduler's
            // `SliceOutcome::Wait` arm (`scheduler/execution/core.rs`): the
            // post-store mailbox re-check runs *before* the pid is inserted
            // into the wait set, and `wake_process` no-ops for unregistered
            // pids, so a wake marker delivered inside that gap parks the
            // process until the next delivery. It is unreachable from the
            // embedder's side (no public API synchronizes with the wait-set
            // insert) and self-heals on the next delivery by construction —
            // which is exactly what this arm verifies: the immediate retry
            // must be serviced and must surface the correct HandlerFailed.
            Err(EngineError::Query(QueryError::Timeout)) => {
                lost_wakeup_timeouts += 1;
                match engine.query(&workflow_id, &run_id, "boom").await {
                    Err(EngineError::Query(QueryError::HandlerFailed { .. })) => {}
                    other => wrong.push((iteration, format!("retry after timeout: {other:?}"))),
                }
            }
            other => wrong.push((iteration, format!("{other:?}"))),
        }
    }
    assert!(
        lost_wakeup_timeouts <= 4,
        "lost-wakeup timeouts beyond the known beamr window rate \
         ({lost_wakeup_timeouts}/{BOOM_ITERATIONS}); the pre-fix reply-path \
         defect killed the workflow within the first handful of iterations"
    );
    stop.store(true, std::sync::atomic::Ordering::Release);
    let mut background_failures = Vec::new();
    for task in background {
        background_failures.extend(task.await?);
    }

    assert!(
        wrong.is_empty(),
        "a failing handler must always surface HandlerFailed; deviations: {wrong:?}"
    );
    assert!(
        background_failures.is_empty(),
        "concurrent state bursts against live workflows must never error: \
         {background_failures:?}"
    );
    // The hammered workflow is still alive and healthy: it answers and
    // completes normally, and the query path appended nothing.
    let after = store.read_history(&workflow_id).await?;
    assert_eq!(
        after.len(),
        1,
        "queries must never append events: {after:#?}"
    );
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
        .map_err(|error| format!("workflow failed after stress: {error:?}"))?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(result.bytes())?,
        json!(42)
    );
    engine.shutdown()?;
    Ok(())
}
