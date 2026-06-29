//! `run_server` durable-outbox bootstrap integration tests.
//!
//! The tests launch the public `aion_server::run::run` entrypoint in a child
//! copy of this integration-test executable. That keeps the production
//! bootstrap intact (config load, `ServerState::build`, outbox dispatcher gate,
//! HTTP/gRPC transports) without adding public test APIs or manually spawning an
//! `OutboxDispatcher`.
//!
//! The libSQL store is now an opt-in backend (`libsql-backend`); this end-to-end
//! boots a real server over `store.backend = libsql`, so it compiles and runs
//! only under that feature. Run it with
//! `cargo test -p aion-server --features libsql-backend`.
#![cfg(feature = "libsql-backend")]

#[path = "run_server_outbox_support/helpers.rs"]
mod helpers;
#[path = "run_server_outbox_support/worker.rs"]
mod worker;

use std::path::PathBuf;
use std::process::ExitCode;

use aion_core::Event;
use aion_server::config::CliOverrides;
use aion_store::{OutboxRow, OutboxStatus, ReadableEventStore};
use aion_store_libsql::LibSqlStore;
use chrono::{SecondsFormat, Utc};
use helpers::{
    FAN_OUT, TestError, assert_fan_out_settled, assert_task_set, count_completed,
    count_completed_for, count_kind, run_server_harness, run_server_harness_with_reconciliation,
    start_over_http, task_ordinal, test_error, unique_temp_dir, wait_for_history, wait_for_rows,
    worker_result, write_package_archive,
};
use worker::WorkerSession;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_server_outbox_happy_path_fan_out_completes_once() -> Result<(), TestError> {
    let dir = unique_temp_dir("happy")?;
    let db_path = dir.path().join("aion.db");
    let package_path = write_package_archive(dir.path())?;
    let (server, http, grpc) = run_server_harness(dir.path(), &db_path, &package_path).await?;
    let reader = LibSqlStore::open(db_path).await?;

    let (workflow_id, run_id) = start_over_http(http).await?;
    wait_for_history(&reader, &workflow_id, "fan-out scheduled", |events| {
        count_kind(events, |event| {
            matches!(event, Event::ActivityScheduled { .. })
        }) == FAN_OUT
    })
    .await?;
    // Rows are staged and not yet dispatched/done while no worker is connected.
    // LSUB-2 collapses the stage->claim latency: the advisory wake fires the
    // dispatcher the instant `append_with_outbox` commits, so the rows may already
    // be `Claimed` (rather than lingering `Pending` for up to a poll interval) by
    // the time this read lands. Either pre-dispatch state proves "staged before
    // worker registration"; the dedicated `Claimed` wait below pins that the
    // dispatcher then claims them while waiting for a worker.
    wait_for_rows(
        &reader,
        &workflow_id,
        &[0, 1, 2, 3],
        "rows staged (pending or claimed) before worker registration",
        |statuses| {
            statuses
                .iter()
                .all(|status| matches!(status, OutboxStatus::Pending | OutboxStatus::Claimed))
        },
    )
    .await?;
    wait_for_rows(
        &reader,
        &workflow_id,
        &[0, 1, 2, 3],
        "dispatcher claimed a row while waiting for workers",
        |statuses| statuses.contains(&OutboxStatus::Claimed),
    )
    .await?;
    let mut worker = WorkerSession::connect(grpc).await?;

    let mut tasks = Vec::with_capacity(FAN_OUT);
    for _ in 0..FAN_OUT {
        tasks.push(worker.next_task().await?);
    }
    assert_task_set(&tasks, &[0, 1, 2, 3])?;
    wait_for_rows(
        &reader,
        &workflow_id,
        &[0, 1, 2, 3],
        "rows dispatched",
        |statuses| statuses.iter().all(|status| *status == OutboxStatus::Done),
    )
    .await?;

    for task in &tasks {
        let ordinal = task_ordinal(task)?;
        worker
            .complete(task, worker_result(ordinal).as_bytes())
            .await?;
    }
    let history = assert_fan_out_settled(&reader, &workflow_id).await?;
    assert_eq!(count_completed(&history), FAN_OUT);
    std::hint::black_box(run_id);
    server.stop()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_server_outbox_restart_rearms_stranded_rows() -> Result<(), TestError> {
    let dir = unique_temp_dir("restart")?;
    let db_path = dir.path().join("aion.db");
    let package_path = write_package_archive(dir.path())?;
    let (server1, http1, grpc1) = run_server_harness(dir.path(), &db_path, &package_path).await?;
    let reader = LibSqlStore::open(db_path.clone()).await?;
    let mut worker1 = WorkerSession::connect(grpc1).await?;

    let (workflow_id, run_id) = start_over_http(http1).await?;
    let mut tasks = Vec::with_capacity(FAN_OUT);
    for _ in 0..FAN_OUT {
        tasks.push(worker1.next_task().await?);
    }
    tasks.sort_by_key(|task| task_ordinal(task).unwrap_or(u64::MAX));
    assert_task_set(&tasks, &[0, 1, 2, 3])?;
    wait_for_rows(
        &reader,
        &workflow_id,
        &[0, 1, 2, 3],
        "initial rows done",
        |statuses| statuses.iter().all(|status| *status == OutboxStatus::Done),
    )
    .await?;

    complete_recorded_prefix(&worker1, &tasks).await?;
    wait_for_history(
        &reader,
        &workflow_id,
        "ordinals 0 and 1 recorded",
        |events| count_completed_for(events, 0) == 1 && count_completed_for(events, 1) == 1,
    )
    .await?;
    let pre_restart = reader.read_history(&workflow_id).await?;
    assert_eq!(count_completed_for(&pre_restart, 2), 0);
    assert_eq!(count_completed_for(&pre_restart, 3), 0);
    drop(worker1);
    server1.stop()?;

    let (server2, http2, grpc2) = run_server_harness(dir.path(), &db_path, &package_path).await?;
    std::hint::black_box(http2);
    wait_for_rows(
        &reader,
        &workflow_id,
        &[2, 3],
        "stranded rows re-armed",
        |statuses| {
            statuses.iter().all(|status| {
                matches!(
                    status,
                    OutboxStatus::Pending | OutboxStatus::Claimed | OutboxStatus::Done
                )
            }) && statuses.iter().any(|status| *status != OutboxStatus::Done)
        },
    )
    .await?;
    let mut worker2 = WorkerSession::connect(grpc2).await?;
    let revived = collect_revived_tasks(&mut worker2).await?;
    complete_with_duplicate_first(&worker2, &revived).await?;

    let history = assert_fan_out_settled(&reader, &workflow_id).await?;
    assert_eq!(count_completed(&history), FAN_OUT);
    std::hint::black_box(run_id);
    server2.stop()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_server_outbox_live_reconciliation_rearms_stranded_claims_once() -> Result<(), TestError>
{
    let dir = unique_temp_dir("live-reconcile")?;
    let db_path = dir.path().join("aion.db");
    let package_path = write_package_archive(dir.path())?;
    let (server, http, grpc) = run_server_harness_with_reconciliation(
        dir.path(),
        &db_path,
        &package_path,
        Some((50, 100)),
    )
    .await?;
    let reader = LibSqlStore::open(db_path.clone()).await?;
    let mut worker = WorkerSession::connect(grpc).await?;

    let (workflow_id, run_id) = start_over_http(http).await?;
    let mut tasks = Vec::with_capacity(FAN_OUT);
    for _ in 0..FAN_OUT {
        tasks.push(worker.next_task().await?);
    }
    tasks.sort_by_key(|task| task_ordinal(task).unwrap_or(u64::MAX));
    assert_task_set(&tasks, &[0, 1, 2, 3])?;
    wait_for_rows(
        &reader,
        &workflow_id,
        &[0, 1, 2, 3],
        "initial rows done before simulated stale claim",
        |statuses| statuses.iter().all(|status| *status == OutboxStatus::Done),
    )
    .await?;

    complete_recorded_prefix(&worker, &tasks).await?;
    wait_for_history(
        &reader,
        &workflow_id,
        "ordinals 0 and 1 recorded before live reconciliation",
        |events| count_completed_for(events, 0) == 1 && count_completed_for(events, 1) == 1,
    )
    .await?;

    force_rows_to_stale_claimed(&db_path, &workflow_id, &[2, 3]).await?;

    let revived = collect_revived_tasks(&mut worker).await?;
    complete_with_duplicate_first(&worker, &revived).await?;
    complete_original_late(&worker, &tasks[2..]).await?;

    let history = assert_fan_out_settled(&reader, &workflow_id).await?;
    assert_eq!(count_completed(&history), FAN_OUT);
    assert_eq!(
        count_kind(&history, |event| matches!(
            event,
            Event::WorkflowCompleted { .. }
        )),
        1
    );
    std::hint::black_box(run_id);
    server.stop()?;
    Ok(())
}

async fn complete_recorded_prefix(
    worker: &WorkerSession,
    tasks: &[aion_proto::generated::ActivityTask],
) -> Result<(), TestError> {
    for task in tasks.iter().take(2) {
        let ordinal = task_ordinal(task)?;
        worker
            .complete(task, worker_result(ordinal).as_bytes())
            .await?;
    }
    Ok(())
}

async fn collect_revived_tasks(
    worker: &mut WorkerSession,
) -> Result<Vec<aion_proto::generated::ActivityTask>, TestError> {
    let mut revived = Vec::with_capacity(2);
    for _ in 0..2 {
        revived.push(worker.next_task().await?);
    }
    revived.sort_by_key(|task| task_ordinal(task).unwrap_or(u64::MAX));
    assert_task_set(&revived, &[2, 3])?;
    Ok(revived)
}

async fn complete_with_duplicate_first(
    worker: &WorkerSession,
    revived: &[aion_proto::generated::ActivityTask],
) -> Result<(), TestError> {
    let first = revived
        .first()
        .ok_or_else(|| test_error("missing first revived task"))?;
    let first_ordinal = task_ordinal(first)?;
    worker
        .complete(first, worker_result(first_ordinal).as_bytes())
        .await?;
    worker
        .complete(first, worker_result(first_ordinal).as_bytes())
        .await?;
    let second = revived
        .get(1)
        .ok_or_else(|| test_error("missing second revived task"))?;
    let second_ordinal = task_ordinal(second)?;
    worker
        .complete(second, worker_result(second_ordinal).as_bytes())
        .await?;
    Ok(())
}

async fn complete_original_late(
    worker: &WorkerSession,
    tasks: &[aion_proto::generated::ActivityTask],
) -> Result<(), TestError> {
    for task in tasks {
        let ordinal = task_ordinal(task)?;
        worker
            .complete(task, worker_result(ordinal).as_bytes())
            .await?;
    }
    Ok(())
}

async fn force_rows_to_stale_claimed(
    db_path: &std::path::Path,
    workflow_id: &aion_core::WorkflowId,
    ordinals: &[u64],
) -> Result<(), TestError> {
    let database = libsql::Builder::new_local(db_path).build().await?;
    let connection = database.connect()?;
    let claimed_at =
        (Utc::now() - chrono::Duration::seconds(60)).to_rfc3339_opts(SecondsFormat::Nanos, true);
    for ordinal in ordinals {
        let dispatch_key = OutboxRow::dispatch_key_for(workflow_id, *ordinal);
        connection
            .execute(
                "UPDATE outbox SET status = 'claimed', claimed_at = ?2 WHERE dispatch_key = ?1",
                libsql::params![dispatch_key, claimed_at.clone()],
            )
            .await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_server_child_process() -> Result<(), TestError> {
    if std::env::var_os("AION_RUN_SERVER_CHILD").is_none() {
        return Ok(());
    }
    let config_path = std::env::var_os("AION_RUN_SERVER_CONFIG")
        .map(PathBuf::from)
        .ok_or_else(|| test_error("AION_RUN_SERVER_CONFIG is required"))?;
    let code = aion_server::run::run(CliOverrides {
        config_path: Some(config_path),
        ..CliOverrides::default()
    })
    .await;
    if code == ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(test_error(format!("run_server exited with {code:?}")))
    }
}
