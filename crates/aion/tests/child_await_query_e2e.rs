//! From-source child-await suspension and query-at-yield-point gates.
//!
//! The `child_query` fixture (`tests/fixtures/child_query/`) is rebuilt from
//! the committed `gleam/aion_flow` source on every run. A parent registers a
//! query handler, spawns one child, and parks in the SDK's pump-wrapped
//! `child.await`; the child sleeps and completes with a deliberately tiny
//! output so the terminal envelope stays within beamr 0.5.0's inline-binary
//! limit (`erlang:byte_size/1` currently raises badarg on >64-byte refc
//! binaries — the upstream defect pinned in `tests/example_query_reentry.rs`)
//! and these tests isolate the child-await and query semantics themselves.

#[path = "common/example_build.rs"]
mod example_build;

use std::sync::Arc;
use std::time::Duration;

use aion::{Engine, EngineBuilder, EngineError, QueryError};
use aion_core::{Payload, RunId, WorkflowId, WorkflowStatus};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const QUERY_POLL_DEADLINE: Duration = Duration::from_secs(20);

async fn engine_with_fixture(
    store: &Arc<dyn EventStore>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    const FIXTURE: &str = "crates/aion/tests/fixtures/child_query";
    // The parent spawns children by workflow type, so both archives are
    // loaded. The second build is an incremental no-op under the shared
    // per-project build lock.
    let parent = example_build::built_package(FIXTURE, "parent_query")?;
    let child = example_build::built_package(FIXTURE, "child_small")?;
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .query_timeout(Duration::from_secs(5))
        .load_workflows(parent)
        .load_workflows(child)
        .build()
        .await?)
}

async fn start_parent(
    engine: &Engine,
    child_sleep_ms: u64,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let input = Payload::from_json(&json!({ "sleep_ms": child_sleep_ms }))?;
    let handle = engine
        .start_workflow(
            "parent_query",
            input,
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
) -> Result<Payload, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + QUERY_POLL_DEADLINE;
    loop {
        match engine.query(workflow_id, run_id, name).await {
            Err(EngineError::Query(QueryError::UnknownQuery(_)))
                if std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            outcome => return Ok(outcome?),
        }
    }
}

async fn assert_parent_completes(
    engine: &Engine,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> TestResult {
    let result = engine.result(workflow_id, run_id).await?;
    let history = store.read_history(workflow_id).await?;
    let payload =
        result.map_err(|error| format!("parent failed: {error:?}\nhistory: {history:#?}"))?;
    let output: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(output, json!("child:done"), "history: {history:#?}");
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Completed
    );
    Ok(())
}

/// Child-await baseline: spawn one child, park in `child.await`, decode the
/// recorded terminal, complete. Before the 0.3.0 SDK fix the child-terminal
/// wake crashed the parent (`bad function term {ok, <<...>>}`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parent_awaits_child_and_completes() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with_fixture(&store).await?;
    let (workflow_id, run_id) = start_parent(&engine, 300).await?;

    assert_parent_completes(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

/// AT-007 C20 at the child-await yield point: queries are answered while the
/// parent is parked in `child.await`, append no history, and the run still
/// completes normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_answered_while_parked_in_child_await() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with_fixture(&store).await?;
    let (workflow_id, run_id) = start_parent(&engine, 3000).await?;

    let reply = query_when_registered(&engine, &workflow_id, &run_id, "phase").await?;
    let value: serde_json::Value = serde_json::from_slice(reply.bytes())?;
    assert_eq!(value, json!("awaiting-child"));

    let parked = store.read_history(&workflow_id).await?;
    for _ in 0..3 {
        let reply = query_when_registered(&engine, &workflow_id, &run_id, "phase").await?;
        let value: serde_json::Value = serde_json::from_slice(reply.bytes())?;
        assert_eq!(value, json!("awaiting-child"));
    }
    assert_eq!(
        store.read_history(&workflow_id).await?,
        parked,
        "the query path must never append events"
    );

    assert_parent_completes(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}
