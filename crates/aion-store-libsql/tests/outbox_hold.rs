//! Pause dispatch-hold (#204) route-starvation regression for the libSQL outbox.
//!
//! The held-workflow filter runs in Rust after the SELECT. With a SQL LIMIT, a
//! paused workflow whose due rows sort earliest would permanently fill the whole
//! claim window and the sweep would claim ZERO rows for the route — every
//! queue-mate of a paused >=window-size fan-out stalls for the entire pause.
//! These tests pin the fix: the excluding claims stream past held rows and still
//! return other workflows' due rows, on both the unscoped and the scoped
//! (backpressure) paths.

// Test code asserts via expect/unwrap exactly like the crate's other test
// binaries (conformance, persistence); allow the restriction lints the same way.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_core::{ContentType, Payload, WorkflowId};
use aion_store::{ClaimScope, OutboxRow, OutboxStore};
use aion_store_libsql::LibSqlStore;
use chrono::{Duration, Utc};

static DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let counter = DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "aion-store-libsql-outbox-hold-{name}-{}-{nanos}-{counter}.db",
        std::process::id()
    ))
}

fn payload() -> Payload {
    Payload::new(ContentType::Json, b"{}".to_vec())
}

/// Stage `paused_rows` due rows for a (to-be-held) workflow that sort EARLIER
/// than one due row for an unrelated workflow, and return both ids.
async fn stage_starvation_backlog(
    store: &LibSqlStore,
    paused_rows: u64,
) -> (WorkflowId, WorkflowId) {
    let paused = WorkflowId::new_v4();
    let other = WorkflowId::new_v4();
    let earlier = Utc::now() - Duration::seconds(60);
    let later = Utc::now() - Duration::seconds(5);

    let mut rows: Vec<OutboxRow> = (0..paused_rows)
        .map(|ordinal| {
            OutboxRow::pending(
                paused.clone(),
                ordinal,
                String::from("charge"),
                payload(),
                earlier,
            )
        })
        .collect();
    rows.push(OutboxRow::pending(
        other.clone(),
        0,
        String::from("charge"),
        payload(),
        later,
    ));
    store
        .append_outbox_batch(&rows)
        .await
        .expect("append outbox rows");

    (paused, other)
}

/// Unscoped sweep: a paused workflow with more due rows than the claim window
/// must not starve the route — the other workflow's row is still claimed.
#[tokio::test]
async fn unscoped_excluding_claim_survives_held_backlog_wider_than_window() {
    let store = LibSqlStore::open(unique_temp_path("unscoped"))
        .await
        .expect("open store");
    let window: u32 = 16;
    let (paused, other) = stage_starvation_backlog(&store, u64::from(window) + 4).await;

    let held: HashSet<WorkflowId> = HashSet::from([paused]);
    let claimed = store
        .claim_outbox_rows_excluding(window, &held)
        .await
        .expect("excluding claim");

    let claimed_ids: Vec<&WorkflowId> = claimed.iter().map(|row| &row.workflow_id).collect();
    assert_eq!(
        claimed_ids,
        vec![&other],
        "the non-held workflow's due row must be claimed even though the held \
         backlog fills the SQL window"
    );

    // The held rows stay Pending: releasing the hold makes them claimable again.
    let released = store
        .claim_outbox_rows_excluding(window, &HashSet::new())
        .await
        .expect("post-release claim");
    assert_eq!(
        released.len(),
        window as usize,
        "released rows are claimable by the ordinary sweep"
    );
}

/// Scoped (backpressure) sweep: same guarantee through the scoped claim path.
#[tokio::test]
async fn scoped_excluding_claim_survives_held_backlog_wider_than_window() {
    let store = LibSqlStore::open(unique_temp_path("scoped"))
        .await
        .expect("open store");
    let window: u32 = 16;
    let (paused, other) = stage_starvation_backlog(&store, u64::from(window) + 4).await;

    let scope = ClaimScope::new("default", "default");
    let held: HashSet<WorkflowId> = HashSet::from([paused]);
    let claimed = store
        .claim_outbox_rows_scoped_excluding(&scope, window, &held)
        .await
        .expect("scoped excluding claim");

    let claimed_ids: Vec<&WorkflowId> = claimed.iter().map(|row| &row.workflow_id).collect();
    assert_eq!(
        claimed_ids,
        vec![&other],
        "the non-held workflow's due row must be claimed through the scoped \
         (backpressure) path even though the held backlog fills the SQL window"
    );
}
