//! Single-process MULTI-SHARD correctness for `HaematiteStore` (AA-4-2).
//!
//! Builds a store with `shard_count == 3` and drives the full adapter surface
//! across workflows that provably route to MORE THAN ONE haematite shard. The
//! point is the cross-shard fan-out scan in `scan_prefix`: every enumeration
//! (`list_workflow_ids`, `list_active`, `query`, `expired_timers`,
//! `claim_outbox_rows`, `list_packages`) must return records from ALL shards,
//! not just the one the scan's lower bound happens to route to. Under the old
//! shard-LOCAL scan these would silently miss records on the other shards; the
//! count assertions below would then fail.

// Test code asserts via expect/unwrap exactly like the crate's other test
// binaries (conformance, distributed_failover); allow the restriction lints
// here the same way.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_core::{
    ContentType, Event, EventEnvelope, PackageVersion, Payload, RunId, TimerId, WorkflowFilter,
    WorkflowId,
};
use aion_store::{
    OutboxRow, OutboxStatus, OutboxStore, PackageRecord, PackageStore, ReadableEventStore,
    WritableEventStore, WriteToken,
};
use aion_store_haematite::HaematiteStore;
use chrono::{Duration, Utc};

const SHARD_COUNT: usize = 3;
const WORKFLOWS: usize = 6;
const PACKAGES: u32 = 4;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "aion-store-haematite-multishard-{name}-{}-{nanos}-{counter}",
        std::process::id()
    ))
}

/// Reproduce the adapter's PRIVATE `keyspace::event_stream_key` encoding (`E`
/// tag byte followed by the raw 16-byte UUID) so the test can ask haematite
/// which shard a workflow's event stream routes to, WITHOUT touching production
/// key encoding.
fn event_stream_key(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 16);
    key.push(b'E');
    key.extend_from_slice(workflow_id.as_uuid().as_bytes());
    key
}

/// Reproduce the adapter's PRIVATE `keyspace::package_key` encoding
/// (`p:` || `workflow_type` || `0x1f` || `content_hash`) to check
/// package-region sharding.
fn package_key(workflow_type: &str, content_hash: &str) -> Vec<u8> {
    let mut key = b"p:".to_vec();
    key.extend_from_slice(workflow_type.as_bytes());
    key.push(0x1f);
    key.extend_from_slice(content_hash.as_bytes());
    key
}

fn started_event(workflow_id: &WorkflowId, seq: u64) -> Event {
    Event::WorkflowStarted {
        envelope: EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: String::from("checkout"),
        input: Payload::new(ContentType::Json, b"{}".to_vec()),
        run_id: RunId::new_v4(),
        parent_run_id: None,
        package_version: PackageVersion::new("a".repeat(64)),
    }
}

/// Build a `shard_count == 3` store and append one started event for a set of
/// workflows whose event streams PROVABLY span more than one shard. Returns the
/// store and the workflow ids. Asserting `>= 2` distinct shards keeps the
/// fan-out proof non-vacuous: under a shard-local scan, enumerations would miss
/// the workflows on the other shards.
fn multishard_store_with_workflows(name: &str) -> (HaematiteStore, Vec<WorkflowId>) {
    let store =
        HaematiteStore::create_with_shard_count(unique_dir(name), SHARD_COUNT).expect("create");

    let database = store.event_store().database();
    let mut workflows: Vec<WorkflowId> = Vec::new();
    let mut shards_hit: BTreeSet<usize> = BTreeSet::new();
    while workflows.len() < WORKFLOWS || shards_hit.len() < 2 {
        let workflow_id = WorkflowId::new_v4();
        shards_hit.insert(database.shard_for(&event_stream_key(&workflow_id)));
        workflows.push(workflow_id);
    }
    assert!(
        shards_hit.len() >= 2,
        "test must span >1 shard to be a real fan-out proof; hit shards: {shards_hit:?}"
    );
    (store, workflows)
}

async fn append_started(store: &HaematiteStore, workflow_id: &WorkflowId) {
    store
        .append(
            WriteToken::recorder(),
            workflow_id,
            std::slice::from_ref(&started_event(workflow_id, 1)),
            0,
        )
        .await
        .expect("append");
}

/// Per-stream routing + the core enumeration fan-out: every workflow's stream is
/// readable, and `list_workflow_ids` / `list_active` / `query` each return ALL
/// workflows across shards — not just the lower bound's shard.
#[tokio::test(flavor = "multi_thread")]
async fn enumerations_fan_out_across_shards() {
    let (store, workflows) = multishard_store_with_workflows("enum");
    for workflow_id in &workflows {
        append_started(&store, workflow_id).await;
    }

    for workflow_id in &workflows {
        assert_eq!(
            store.read_history(workflow_id).await.expect("history").len(),
            1,
            "each workflow's stream is readable on its own shard"
        );
    }

    let mut listed = store.list_workflow_ids().await.expect("list ids");
    listed.sort_by_key(ToString::to_string);
    let mut expected = workflows.clone();
    expected.sort_by_key(ToString::to_string);
    assert_eq!(
        listed, expected,
        "list_workflow_ids must return ALL workflows across shards"
    );

    let active = store.list_active().await.expect("list active");
    assert_eq!(
        active.len(),
        workflows.len(),
        "list_active must see every running workflow across shards"
    );

    let summaries = store
        .query(&WorkflowFilter::default())
        .await
        .expect("query");
    assert_eq!(
        summaries.len(),
        workflows.len(),
        "query must return summaries from every shard"
    );
}

/// `expired_timers` fans out: one timer scheduled per workflow (which span >1
/// shard), and the due scan returns every one of them.
#[tokio::test(flavor = "multi_thread")]
async fn expired_timers_fan_out_across_shards() {
    let (store, workflows) = multishard_store_with_workflows("timers");
    for workflow_id in &workflows {
        append_started(&store, workflow_id).await;
    }

    let fire_at = Utc::now();
    for (ordinal, workflow_id) in workflows.iter().enumerate() {
        let timer_id = TimerId::anonymous(u64::try_from(ordinal).unwrap());
        store
            .schedule_timer(workflow_id, &timer_id, fire_at)
            .await
            .expect("schedule timer");
    }

    let expired = store
        .expired_timers(fire_at + Duration::seconds(1))
        .await
        .expect("expired timers");
    assert_eq!(
        expired.len(),
        workflows.len(),
        "expired_timers must fan out across shards and return every due timer"
    );
}

/// Outbox fan-out: rows written for workflows on different shards (through both
/// `append_with_outbox` and `append_outbox_batch`), then `claim_outbox_rows`
/// claims them ALL across shards, and `complete_outbox_row` transitions one.
#[tokio::test(flavor = "multi_thread")]
async fn outbox_claims_fan_out_across_shards() {
    let (store, workflows) = multishard_store_with_workflows("outbox");
    for workflow_id in &workflows {
        append_started(&store, workflow_id).await;
    }

    let past = Utc::now() - Duration::hours(1);
    for (index, workflow_id) in workflows.iter().enumerate() {
        let row = OutboxRow::pending(
            workflow_id.clone(),
            0,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            past,
        );
        if index % 2 == 0 {
            // append_with_outbox alongside no new events (expected_seq == 1
            // matches the single started event already appended).
            store
                .append_with_outbox(
                    WriteToken::recorder(),
                    workflow_id,
                    &[],
                    1,
                    std::slice::from_ref(&row),
                )
                .await
                .expect("append_with_outbox");
        } else {
            store
                .append_outbox_batch(std::slice::from_ref(&row))
                .await
                .expect("append_outbox_batch");
        }
    }

    let claimed = store
        .claim_outbox_rows(u32::try_from(workflows.len() * 2).unwrap())
        .await
        .expect("claim");
    assert_eq!(
        claimed.len(),
        workflows.len(),
        "claim_outbox_rows must claim rows from every shard"
    );
    assert!(claimed.iter().all(|row| row.status == OutboxStatus::Claimed));

    store
        .complete_outbox_row(&claimed[0].dispatch_key)
        .await
        .expect("complete");
    // The completed row is no longer claimable; the remainder still are not
    // (already Claimed), so a re-claim yields nothing.
    assert!(
        store
            .claim_outbox_rows(100)
            .await
            .expect("reclaim")
            .is_empty(),
        "completed/claimed rows are not re-claimable"
    );
}

/// Package fan-out: several packages whose keys provably land on >1 shard, and
/// `list_packages` returns all of them.
#[tokio::test(flavor = "multi_thread")]
async fn list_packages_fans_out_across_shards() {
    let store =
        HaematiteStore::create_with_shard_count(unique_dir("packages"), SHARD_COUNT).expect("create");
    let database = store.event_store().database();

    let mut package_shards: BTreeSet<usize> = BTreeSet::new();
    for index in 0..PACKAGES {
        let workflow_type = format!("type-{index}");
        let content_hash = format!("{index:0>64}");
        package_shards.insert(database.shard_for(&package_key(&workflow_type, &content_hash)));
        store
            .put_package(PackageRecord {
                workflow_type,
                content_hash,
                archive: b"archive".to_vec(),
                deployed_at: Utc::now(),
            })
            .await
            .expect("put package");
    }
    assert!(
        package_shards.len() >= 2,
        "package records should span >1 shard; hit: {package_shards:?}"
    );

    let packages = store.list_packages().await.expect("list packages");
    assert_eq!(
        packages.len(),
        PACKAGES as usize,
        "list_packages must return every package across shards"
    );
}
