//! End-to-end pause/resume integration tests over `InMemoryStore` (#204).
//!
//! Pause is a durable, operator-facing hold on new activity dispatch for a live,
//! non-terminal run; resume releases it. These tests exercise the engine
//! `pause_workflow` / `resume_paused_workflow` ops over a real engine: the typed
//! rejections that mirror reopen's `InvalidState` family (GATE-3), the durable
//! recovery-exclusion + respawn-on-resume path for a run paused across a restart
//! (GATE-2), and the invariant that a rejection appends nothing to history.

mod common;

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use aion::{Engine, EngineBuilder, EngineError};
use aion_core::{Event, EventEnvelope, RunId, WorkflowId, WorkflowStatus, status_from_events};
use aion_store::{EventStore, InMemoryStore, WriteToken};
use chrono::Utc;

use common::{FIXTURE_MODULE, fixture_package, input_payload};

fn envelope(workflow_id: &WorkflowId, seq: u64) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: Utc::now(),
        workflow_id: workflow_id.clone(),
    }
}

fn started(
    workflow_id: &WorkflowId,
    run_id: &RunId,
    hash: &str,
) -> Result<Event, Box<dyn std::error::Error>> {
    Ok(Event::WorkflowStarted {
        envelope: envelope(workflow_id, 1),
        workflow_type: FIXTURE_MODULE.to_owned(),
        input: input_payload()?,
        run_id: run_id.clone(),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new(hash.to_owned()),
    })
}

async fn seed(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    events: Vec<Event>,
) -> Result<(), Box<dyn std::error::Error>> {
    store
        .append(WriteToken::recorder(), workflow_id, &events, 0)
        .await?;
    Ok(())
}

async fn engine_over(
    store: &Arc<dyn EventStore>,
    entry_function: &str,
) -> Result<(Engine, String), Box<dyn std::error::Error>> {
    let package = fixture_package(entry_function)?;
    let hash = package.content_hash().to_string();
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?;
    Ok((engine, hash))
}

/// Asserts `result` is a typed `InvalidState` rejection whose message names
/// `needle` — never using a panic/`expect` (restriction-lint clean).
fn assert_invalid_state<T: Debug>(
    result: Result<T, EngineError>,
    needle: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match result {
        Err(EngineError::InvalidState { reason }) => {
            if reason.contains(needle) {
                Ok(())
            } else {
                Err(format!("rejection must name {needle}, got: {reason}").into())
            }
        }
        other => Err(format!("expected InvalidState naming {needle}, got {other:?}").into()),
    }
}

/// GATE-3: `pause` of a `Completed` run is refused naming `Completed`.
#[tokio::test]
async fn pause_of_completed_names_completed() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (engine, hash) = engine_over(&store, "complete").await?;
    let (id, run) = (WorkflowId::new_v4(), RunId::new_v4());
    seed(
        &store,
        &id,
        vec![
            started(&id, &run, &hash)?,
            Event::WorkflowCompleted {
                envelope: envelope(&id, 2),
                result: input_payload()?,
            },
        ],
    )
    .await?;
    let before = store.read_history(&id).await?.len();
    assert_invalid_state(
        engine.pause_workflow(&id, &run, None, None).await,
        "Completed",
    )?;
    assert_eq!(
        store.read_history(&id).await?.len(),
        before,
        "a rejected pause appends nothing"
    );
    engine.shutdown()?;
    Ok(())
}

/// GATE-3: `pause` of an already-`Paused` run is refused naming `Paused`.
#[tokio::test]
async fn pause_of_paused_names_paused() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (engine, hash) = engine_over(&store, "complete").await?;
    let (id, run) = (WorkflowId::new_v4(), RunId::new_v4());
    seed(
        &store,
        &id,
        vec![
            started(&id, &run, &hash)?,
            Event::WorkflowPaused {
                envelope: envelope(&id, 2),
                run_id: run.clone(),
                reason: None,
                operator: None,
            },
        ],
    )
    .await?;
    let before = store.read_history(&id).await?.len();
    assert_invalid_state(engine.pause_workflow(&id, &run, None, None).await, "Paused")?;
    assert_eq!(store.read_history(&id).await?.len(), before);
    engine.shutdown()?;
    Ok(())
}

/// GATE-3: `resume` of a `Running` (never-paused) run is refused naming `Running`.
#[tokio::test]
async fn resume_of_running_names_running() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (engine, hash) = engine_over(&store, "complete").await?;
    let (id, run) = (WorkflowId::new_v4(), RunId::new_v4());
    seed(&store, &id, vec![started(&id, &run, &hash)?]).await?;
    let before = store.read_history(&id).await?.len();
    assert_invalid_state(
        engine.resume_paused_workflow(&id, &run, None).await,
        "Running",
    )?;
    assert_eq!(store.read_history(&id).await?.len(), before);
    engine.shutdown()?;
    Ok(())
}

/// GATE-3: `resume` of a `Cancelled` run is refused naming `Cancelled`.
#[tokio::test]
async fn resume_of_cancelled_names_cancelled() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (engine, hash) = engine_over(&store, "complete").await?;
    let (id, run) = (WorkflowId::new_v4(), RunId::new_v4());
    seed(
        &store,
        &id,
        vec![
            started(&id, &run, &hash)?,
            Event::WorkflowCancelled {
                envelope: envelope(&id, 2),
                reason: String::from("operator stop"),
            },
        ],
    )
    .await?;
    let before = store.read_history(&id).await?.len();
    assert_invalid_state(
        engine.resume_paused_workflow(&id, &run, None).await,
        "Cancelled",
    )?;
    assert_eq!(store.read_history(&id).await?.len(), before);
    engine.shutdown()?;
    Ok(())
}

/// GATE-2 (recovery half + resume respawn): a run paused before a restart projects
/// `Paused`, is excluded from `list_active` (so the startup recovery sweep does NOT
/// respawn it) yet is rebuilt into the dispatch-hold set from `list_paused`;
/// issuing `resume` respawns it via the reopen-style `register_recovered_resident`
/// path (history re-read AFTER the `WorkflowResumed` append) and it proceeds to
/// COMPLETE.
#[tokio::test]
async fn paused_run_is_recovery_excluded_then_resumes_and_completes()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    // Seed a durably-`Paused` run BEFORE building the engine — a run that was
    // paused and then survived a `kill -9`.
    let (id, run) = (WorkflowId::new_v4(), RunId::new_v4());
    let package = fixture_package("complete")?;
    let hash = package.content_hash().to_string();
    seed(
        &store,
        &id,
        vec![
            started(&id, &run, &hash)?,
            Event::SearchAttributesUpdated {
                envelope: envelope(&id, 2),
                workflow_id: id.clone(),
                attributes: HashMap::from([(
                    String::from("aion.namespace"),
                    aion_core::SearchAttributeValue::String(String::from("default")),
                )]),
            },
            Event::WorkflowPaused {
                envelope: envelope(&id, 3),
                run_id: run.clone(),
                reason: Some(String::from("operator hold")),
                operator: None,
            },
        ],
    )
    .await?;

    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .load_workflows(package)
        .build()
        .await?;

    // The startup recovery sweep does NOT respawn a paused run.
    if engine.registry().get(&id, &run)?.is_some() {
        return Err("a paused run must not be respawned at startup".into());
    }
    // The durable hold is rebuilt from `list_paused`.
    engine.rebuild_paused_runs().await?;
    if !engine.paused_runs().snapshot().contains(&id) {
        return Err("the dispatch-hold set must be rebuilt from list_paused".into());
    }
    assert_eq!(
        status_from_events(&store.read_history(&id).await?),
        WorkflowStatus::Paused
    );

    // Resume respawns via the reopen recovery path and releases the hold.
    let handle = engine.resume_paused_workflow(&id, &run, None).await?;
    assert_eq!(handle.run_id(), &run);
    if engine.paused_runs().snapshot().contains(&id) {
        return Err("resume must release the dispatch hold".into());
    }

    // The resumed run proceeds to COMPLETE.
    let result = engine.result(&id, &run).await?;
    result.map_err(|error| format!("resumed run should complete: {error:?}"))?;
    assert_eq!(
        status_from_events(&store.read_history(&id).await?),
        WorkflowStatus::Completed
    );
    if !store
        .read_history(&id)
        .await?
        .iter()
        .any(|event| matches!(event, Event::WorkflowResumed { .. }))
    {
        return Err("resume must durably record WorkflowResumed".into());
    }

    engine.shutdown()?;
    Ok(())
}

/// GATE-4 (recovery slice): a `Cancelled` run that was earlier paused and resumed
/// still `reopen`s to `Running` — the `WorkflowPaused` / `WorkflowResumed` markers
/// are replay-invisible.
#[tokio::test]
async fn cancelled_formerly_paused_run_reopens_running() -> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let (engine, hash) = engine_over(&store, "complete").await?;
    let (id, run) = (WorkflowId::new_v4(), RunId::new_v4());
    seed(
        &store,
        &id,
        vec![
            started(&id, &run, &hash)?,
            Event::WorkflowPaused {
                envelope: envelope(&id, 2),
                run_id: run.clone(),
                reason: None,
                operator: None,
            },
            Event::WorkflowResumed {
                envelope: envelope(&id, 3),
                run_id: run.clone(),
                operator: None,
            },
            Event::WorkflowCancelled {
                envelope: envelope(&id, 4),
                reason: String::from("operator stop"),
            },
        ],
    )
    .await?;
    assert_eq!(
        status_from_events(&store.read_history(&id).await?),
        WorkflowStatus::Cancelled
    );

    let handle = engine.reopen_workflow(&id, &run).await?;
    assert_eq!(handle.cached_status(), WorkflowStatus::Running);
    engine.shutdown()?;
    Ok(())
}

/// Unknown workflow -> `WorkflowNotFound` for both ops.
#[tokio::test]
async fn pause_and_resume_of_unknown_workflow_is_not_found()
-> Result<(), Box<dyn std::error::Error>> {
    let (engine, _store) = common::engine_with_fixture("complete").await?;
    let (missing, missing_run) = (WorkflowId::new_v4(), RunId::new_v4());
    if !matches!(
        engine
            .pause_workflow(&missing, &missing_run, None, None)
            .await,
        Err(EngineError::WorkflowNotFound { .. })
    ) {
        return Err("pause of unknown must be WorkflowNotFound".into());
    }
    if !matches!(
        engine
            .resume_paused_workflow(&missing, &missing_run, None)
            .await,
        Err(EngineError::WorkflowNotFound { .. })
    ) {
        return Err("resume of unknown must be WorkflowNotFound".into());
    }
    engine.shutdown()?;
    Ok(())
}
