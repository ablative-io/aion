//! End-to-end reopen integration tests over `InMemoryStore`.
//!
//! Reopen turns a terminal-`Failed` or terminal-`Cancelled` run back into a
//! running one that re-drives from where it left off. These tests exercise the
//! full `Engine::reopen_workflow` path — validate precondition, compute the
//! reopened set, append `WorkflowReopened` through one continuous recorder,
//! respawn and register the SAME run as a Resident — over a real engine and the
//! production AD recovery seam.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use aion::{EngineBuilder, EngineError};
use aion_core::{
    ActivityError, ActivityErrorKind, ActivityId, Event, EventEnvelope, RunId,
    SearchAttributeValue, WorkflowError, WorkflowId, WorkflowStatus, status_from_events,
};
use aion_store::{EventStore, InMemoryStore, WriteToken};
use chrono::Utc;
use serde_json::json;

use common::{FIXTURE_MODULE, fixture_package, input_payload, payload};

/// Seeds a crashed terminal-Failed `wait`-fixture run into `store`: a
/// `WorkflowStarted`, a scheduled + terminally-failed `dev_review` activity, and
/// the `WorkflowFailed` terminal — the state a workflow reaches when a step dies
/// and the process crashes, exactly what an operator reopens.
async fn seed_failed_run(
    store: &Arc<dyn EventStore>,
    package_hash: &str,
    namespace: &str,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let envelope = |seq: u64| EventEnvelope {
        seq,
        recorded_at: Utc::now(),
        workflow_id: workflow_id.clone(),
    };
    let events = vec![
        Event::WorkflowStarted {
            envelope: envelope(1),
            workflow_type: FIXTURE_MODULE.to_owned(),
            input: input_payload()?,
            run_id: run_id.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new(package_hash.to_owned()),
        },
        Event::SearchAttributesUpdated {
            envelope: envelope(2),
            workflow_id: workflow_id.clone(),
            attributes: HashMap::from([(
                String::from("aion.namespace"),
                SearchAttributeValue::String(namespace.to_owned()),
            )]),
        },
        Event::ActivityScheduled {
            envelope: envelope(3),
            activity_id: ActivityId::from_sequence_position(0),
            activity_type: String::from("dev_review"),
            input: payload(&json!({ "step": "dev_review" }))?,
            task_queue: String::from("default"),
            node: None,
        },
        Event::ActivityFailed {
            envelope: envelope(4),
            activity_id: ActivityId::from_sequence_position(0),
            error: ActivityError {
                kind: ActivityErrorKind::Terminal,
                message: String::from("provider error: rate limited"),
                details: None,
            },
            attempt: 1,
        },
        Event::WorkflowFailed {
            envelope: envelope(5),
            error: WorkflowError {
                message: String::from("norn review failed"),
                details: None,
            },
        },
    ];
    store
        .append(WriteToken::recorder(), &workflow_id, &events, 0)
        .await?;
    Ok((workflow_id, run_id))
}

#[tokio::test]
async fn reopen_failed_workflow_supersedes_terminal_and_re_drives_running()
-> Result<(), Box<dyn std::error::Error>> {
    let package = fixture_package("wait")?;
    let hash = package.content_hash().to_string();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (workflow_id, run_id) = seed_failed_run(&store, &hash, "default").await?;

    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?;

    // A terminal-Failed workflow is NOT recovered at startup.
    assert!(engine.registry().get(&workflow_id, &run_id)?.is_none());

    let handle = engine.reopen_workflow(&workflow_id, &run_id).await?;
    assert_eq!(handle.workflow_id(), &workflow_id);
    assert_eq!(handle.run_id(), &run_id);
    assert_eq!(handle.cached_status(), WorkflowStatus::Running);

    // The run is now a live resident again.
    assert!(engine.registry().get(&workflow_id, &run_id)?.is_some());

    let history = store.read_history(&workflow_id).await?;
    assert_eq!(status_from_events(&history), WorkflowStatus::Running);
    match history.last() {
        Some(Event::WorkflowReopened {
            run_id: reopened_run,
            reopened,
            ..
        }) => {
            assert_eq!(reopened_run, &run_id);
            assert_eq!(
                reopened,
                &vec![ActivityId::from_sequence_position(0)],
                "the terminally-failed dev_review step must be named for re-dispatch"
            );
        }
        other => return Err(format!("expected trailing WorkflowReopened, found {other:?}").into()),
    }

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn reopen_of_a_failed_run_preserves_namespace_affinity()
-> Result<(), Box<dyn std::error::Error>> {
    // A remote-namespace workflow reopens on its own namespace, never local —
    // the inverse of the 2026-06-15 recovered-remote-routing-local bug.
    let package = fixture_package("wait")?;
    let hash = package.content_hash().to_string();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (workflow_id, run_id) = seed_failed_run(&store, &hash, "remote").await?;

    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?;

    let handle = engine.reopen_workflow(&workflow_id, &run_id).await?;
    assert_eq!(
        handle.namespace(),
        "remote",
        "the reopened run must re-derive its own namespace from history"
    );

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn reopen_rejects_non_reopenable_states() -> Result<(), Box<dyn std::error::Error>> {
    let (engine, store) = common::engine_with_fixture("complete").await?;

    // Unknown workflow -> WorkflowNotFound.
    let missing = WorkflowId::new_v4();
    let missing_run = RunId::new_v4();
    assert!(matches!(
        engine.reopen_workflow(&missing, &missing_run).await,
        Err(EngineError::WorkflowNotFound { .. })
    ));

    // A completed workflow -> InvalidState (Completed is not reopenable).
    let handle = engine
        .start_workflow(
            FIXTURE_MODULE,
            input_payload()?,
            HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    result.map_err(|error| format!("fixture should complete: {error:?}"))?;
    assert_eq!(
        status_from_events(&store.read_history(handle.workflow_id()).await?),
        WorkflowStatus::Completed
    );
    // #223: the rejection must name the run's TRUE state. A completed run can
    // still hold a lingering registry handle, and the handle-first guard used
    // to misreport this exact rejection as "is already Running".
    let rejection = engine
        .reopen_workflow(handle.workflow_id(), handle.run_id())
        .await;
    match rejection {
        Err(EngineError::InvalidState { reason }) => {
            assert!(
                reason.contains("Completed"),
                "the rejection must name the Completed state, got: {reason}"
            );
            assert!(
                !reason.contains("already Running"),
                "a Completed run must not be misreported as Running: {reason}"
            );
        }
        other => return Err(format!("expected InvalidState, got {other:?}").into()),
    }

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn double_reopen_of_a_running_run_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let package = fixture_package("wait")?;
    let hash = package.content_hash().to_string();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (workflow_id, run_id) = seed_failed_run(&store, &hash, "default").await?;

    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?;

    engine.reopen_workflow(&workflow_id, &run_id).await?;
    // The run is now Running; a second reopen must reject, not append a second
    // WorkflowReopened.
    assert!(matches!(
        engine.reopen_workflow(&workflow_id, &run_id).await,
        Err(EngineError::InvalidState { .. })
    ));
    let reopened_count = store
        .read_history(&workflow_id)
        .await?
        .iter()
        .filter(|event| matches!(event, Event::WorkflowReopened { .. }))
        .count();
    assert_eq!(reopened_count, 1, "exactly one WorkflowReopened must exist");

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn restart_after_reopen_recovers_the_run_as_running() -> Result<(), Box<dyn std::error::Error>>
{
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (workflow_id, run_id) = seed_failed_run(
        &store,
        &fixture_package("wait")?.content_hash().to_string(),
        "default",
    )
    .await?;

    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(fixture_package("wait")?)
        .build()
        .await?;
    engine.reopen_workflow(&workflow_id, &run_id).await?;
    engine.shutdown()?;

    // A fresh engine over the same store re-derives the reopened run as Running
    // and re-drives it through the ordinary startup recovery path — no
    // reopen-specific special-casing.
    let recovered = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(fixture_package("wait")?)
        .build()
        .await?;
    let handle = recovered.registry().get(&workflow_id, &run_id)?;
    assert!(
        handle.is_some_and(|handle| handle.cached_status() == WorkflowStatus::Running),
        "the reopened run must recover as a running resident after restart"
    );

    recovered.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn reopen_cancelled_workflow_resumes_from_where_it_was_cancelled()
-> Result<(), Box<dyn std::error::Error>> {
    // AD-013: a deliberately cancelled workflow reopens with an EMPTY reopened
    // set (a cancel records no terminal activity failure) and resumes.
    let (engine, store) = common::engine_with_fixture("wait").await?;
    let handle = engine
        .start_workflow(
            FIXTURE_MODULE,
            input_payload()?,
            HashMap::new(),
            String::from("default"),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();

    engine
        .cancel(&workflow_id, &run_id, "operator stop")
        .await?;
    assert_eq!(
        status_from_events(&store.read_history(&workflow_id).await?),
        WorkflowStatus::Cancelled
    );
    assert!(engine.registry().get(&workflow_id, &run_id)?.is_none());

    let reopened = engine.reopen_workflow(&workflow_id, &run_id).await?;
    assert_eq!(reopened.cached_status(), WorkflowStatus::Running);
    assert!(engine.registry().get(&workflow_id, &run_id)?.is_some());

    let history = store.read_history(&workflow_id).await?;
    assert_eq!(status_from_events(&history), WorkflowStatus::Running);
    match history.last() {
        Some(Event::WorkflowReopened {
            run_id: reopened_run,
            reopened,
            ..
        }) => {
            assert_eq!(reopened_run, &run_id);
            assert!(
                reopened.is_empty(),
                "a cancel-reopen names no step to re-drive, got {reopened:?}"
            );
        }
        other => return Err(format!("expected trailing WorkflowReopened, found {other:?}").into()),
    }

    engine.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn reopened_failed_run_can_fail_and_be_reopened_again()
-> Result<(), Box<dyn std::error::Error>> {
    // Failed -> Reopened -> Failed -> Reopened: the projection is always the last
    // lifecycle event and each lease terminates exactly once.
    let package = fixture_package("wait")?;
    let hash = package.content_hash().to_string();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (workflow_id, run_id) = seed_failed_run(&store, &hash, "default").await?;

    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?;

    engine.reopen_workflow(&workflow_id, &run_id).await?;
    assert_eq!(
        status_from_events(&store.read_history(&workflow_id).await?),
        WorkflowStatus::Running
    );

    // Fail the reopened run again through the engine (kill the resident), then
    // reopen once more — the reset-aware terminal guard permits it.
    engine.cancel(&workflow_id, &run_id, "second stop").await?;
    assert_eq!(
        status_from_events(&store.read_history(&workflow_id).await?),
        WorkflowStatus::Cancelled
    );

    engine.reopen_workflow(&workflow_id, &run_id).await?;
    let history = store.read_history(&workflow_id).await?;
    assert_eq!(status_from_events(&history), WorkflowStatus::Running);
    assert_eq!(
        history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowReopened { .. }))
            .count(),
        2,
        "two reopen markers accumulate across the Failed/Cancelled/Reopened chain"
    );

    engine.shutdown()?;
    Ok(())
}
