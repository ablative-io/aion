//! From-source SDK suspension-protocol regression gates (P0).
//!
//! The `sleep_query` fixture (`tests/fixtures/sleep_query/`) is built from
//! the committed `gleam/aion_flow` source on every run, so these tests
//! validate the CURRENT SDK→engine suspension contract — never a stale
//! prebuilt archive. They pin the release-integrity failure class where the
//! 0.2.0 SDK's pump thunks tail-called the suspending NIFs and every
//! suspending await crashed on wake with `bad function term {ok, <<...>>}`
//! (and a query delivered to a suspended workflow killed the run).

#[path = "common/example_build.rs"]
mod example_build;

use std::sync::Arc;
use std::time::Duration;

use aion::{Engine, EngineBuilder, EngineError, QueryError};
use aion_core::{Event, Payload, RunId, WorkflowId, WorkflowStatus};
use aion_package::Package;
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const QUERY_POLL_DEADLINE: Duration = Duration::from_secs(20);

fn fixture_package() -> Result<Package, Box<dyn std::error::Error>> {
    example_build::built_package("crates/aion/tests/fixtures/sleep_query", "sleep_query")
}

async fn engine_with_fixture(
    store: &Arc<dyn EventStore>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .query_timeout(Duration::from_secs(5))
        .load_workflows(fixture_package()?)
        .build()
        .await?)
}

async fn start_sleeper(
    engine: &Engine,
    sleep_ms: u64,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let input = Payload::from_json(&json!({ "sleep_ms": sleep_ms }))?;
    let handle = engine
        .start_workflow("sleep_query", input, std::collections::HashMap::new())
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

/// Query `name`, retrying while the workflow has not yet executed its
/// registration (registration is workflow code, racing the caller).
async fn query_when_registered(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    name: &str,
) -> Result<Result<Payload, EngineError>, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + QUERY_POLL_DEADLINE;
    loop {
        match engine.query(workflow_id, run_id, name).await {
            Err(EngineError::Query(QueryError::UnknownQuery(_)))
                if std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            outcome => return Ok(outcome),
        }
    }
}

async fn assert_completes_with_slept(
    engine: &Engine,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> TestResult {
    let result = engine.result(workflow_id, run_id).await?;
    let history = store.read_history(workflow_id).await?;
    let payload =
        result.map_err(|error| format!("workflow failed: {error:?}\nhistory: {history:#?}"))?;
    let output: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(output, json!("slept"), "history: {history:#?}");
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Completed
    );
    Ok(())
}

/// The minimal sleep baseline: a workflow whose only await is one durable
/// sleep must complete, and its history must be exactly the four recorded
/// events. Before the 0.3.0 SDK fix this crashed on the timer wake with
/// `bad function term {ok, <<"fired">>}` → `WorkflowFailed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn minimal_sleep_workflow_completes() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with_fixture(&store).await?;
    let (workflow_id, run_id) = start_sleeper(&engine, 500).await?;

    assert_completes_with_slept(&engine, &store, &workflow_id, &run_id).await?;

    let history = store.read_history(&workflow_id).await?;
    let kinds: Vec<bool> = vec![
        matches!(history.first(), Some(Event::WorkflowStarted { .. })),
        matches!(history.get(1), Some(Event::TimerStarted { .. })),
        matches!(history.get(2), Some(Event::TimerFired { .. })),
        matches!(history.get(3), Some(Event::WorkflowCompleted { .. })),
    ];
    assert!(
        history.len() == 4 && kinds.iter().all(|matched| *matched),
        "unexpected history: {history:#?}"
    );

    engine.shutdown()?;
    Ok(())
}

/// AT-007 C20 regression: a query delivered while the workflow is suspended
/// in `workflow.sleep` is answered at the yield point, appends no history,
/// and the run still completes normally. Before the 0.3.0 SDK fix the
/// query's wake killed the suspended run (`bad function term {error, ...}`,
/// caller saw `QueryReplyDropped`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_to_sleeping_workflow_answers_and_run_completes() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with_fixture(&store).await?;
    let (workflow_id, run_id) = start_sleeper(&engine, 3000).await?;

    let reply = query_when_registered(&engine, &workflow_id, &run_id, "status").await??;
    let value: serde_json::Value = serde_json::from_slice(reply.bytes())?;
    assert_eq!(value, json!("sleeping"));

    // Repeated queries re-enter the same parked await; none may append
    // history beyond the recorded sleep start.
    let parked = store.read_history(&workflow_id).await?;
    for _ in 0..3 {
        let reply = query_when_registered(&engine, &workflow_id, &run_id, "status").await??;
        let value: serde_json::Value = serde_json::from_slice(reply.bytes())?;
        assert_eq!(value, json!("sleeping"));
    }
    assert_eq!(
        store.read_history(&workflow_id).await?,
        parked,
        "the query path must never append events"
    );

    assert_completes_with_slept(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

/// A raising query handler must surface as a typed query failure to the
/// caller and must NEVER kill the run: the workflow still completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_query_handler_never_kills_the_run() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with_fixture(&store).await?;
    let (workflow_id, run_id) = start_sleeper(&engine, 3000).await?;

    let failure = query_when_registered(&engine, &workflow_id, &run_id, "boom").await?;
    match failure {
        Err(EngineError::Query(QueryError::HandlerFailed { message })) => {
            assert!(
                message.contains("deliberate handler failure"),
                "unexpected handler failure message: {message}"
            );
        }
        other => {
            return Err(
                format!("a raising handler must surface as HandlerFailed, got: {other:?}").into(),
            );
        }
    }

    // A healthy query still answers after the failed one.
    let reply = query_when_registered(&engine, &workflow_id, &run_id, "status").await??;
    let value: serde_json::Value = serde_json::from_slice(reply.bytes())?;
    assert_eq!(value, json!("sleeping"));

    assert_completes_with_slept(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}
