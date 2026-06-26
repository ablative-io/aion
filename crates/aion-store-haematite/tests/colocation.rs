//! Per-workflow KV CO-LOCATION for `HaematiteStore` (AA-4-3a).
//!
//! AA-4-2 made the adapter correct at `shard_count > 1` by fanning enumeration
//! scans out across every shard. AA-4-3a goes further: every per-workflow KV
//! record (timers, outbox rows) must physically live on the SAME shard as that
//! workflow's event stream, so a future multi-node deployment where a node owns
//! a SUBSET of shards holds exactly its own workflows' timers/outbox. This test
//! proves the physical placement directly via `Database::range_per_shard`:
//!
//! * a workflow's timer + outbox key appear ONLY in its event stream's shard;
//! * two workflows that route to DIFFERENT shards stay isolated (each one's
//!   records on its own shard, neither bleeding onto the other's);
//! * the public API still sees everything despite co-location (fan-out is
//!   complete): `expired_timers` returns both timers, `claim_outbox_rows` claims
//!   both rows, and `complete_outbox_row` / `retry_outbox_row` work (proving the
//!   `dispatch_key` -> `route_key` derivation in `transition_outbox` is correct).

// Test code asserts via expect/unwrap exactly like the crate's other test
// binaries (conformance, distributed_failover, multishard); allow the
// restriction lints here the same way.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::similar_names)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_core::{
    ContentType, Event, EventEnvelope, PackageVersion, Payload, RunId, TimerId, WorkflowId,
};
use aion_store::{
    OutboxRow, OutboxStatus, OutboxStore, ReadableEventStore, WritableEventStore, WriteToken,
};
use aion_store_haematite::HaematiteStore;
use chrono::{Duration, Utc};

const SHARD_COUNT: usize = 3;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "aion-store-haematite-colocation-{name}-{}-{nanos}-{counter}",
        std::process::id()
    ))
}

/// Reproduce the adapter's PRIVATE `keyspace::event_stream_key` encoding (`E`
/// tag byte followed by the raw 16-byte UUID) so the test can ask haematite
/// which shard a workflow's event stream routes to, WITHOUT touching production
/// key encoding. The co-location route key is exactly these bytes.
fn event_stream_key(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 16);
    key.push(b'E');
    key.extend_from_slice(workflow_id.as_uuid().as_bytes());
    key
}

/// Reproduce the adapter's PRIVATE `keyspace::timer_key` PREFIX:
/// `t:` || `workflow_id_text`. Every timer for `workflow_id` shares this prefix
/// (the timer-id token follows after a `0x1f` separator), so its presence in a
/// shard's range proves the workflow's timer landed there.
fn timer_key_prefix(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = b"t:".to_vec();
    key.extend_from_slice(workflow_id.to_string().as_bytes());
    key
}

/// Reproduce the adapter's PRIVATE `keyspace::outbox_key` encoding:
/// `o:` || `dispatch_key`. The `dispatch_key` is canonically "{`workflow_id}:{ordinal`}".
fn outbox_key(dispatch_key: &str) -> Vec<u8> {
    let mut key = b"o:".to_vec();
    key.extend_from_slice(dispatch_key.as_bytes());
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

/// Collect every KV key physically present in `shard`'s `t:`/`o:` regions. We
/// scan the half-open `[b"o:", b"u")` range, which covers both the `o:` (0x6f)
/// and `t:` (0x74) prefixes, on exactly that one shard via `range_per_shard`.
fn shard_kv_keys(database: &haematite::Database, shard: usize) -> Vec<Vec<u8>> {
    database
        .range_per_shard(shard, b"o:", b"u")
        .expect("range_per_shard")
        .into_iter()
        .map(|(key, _value)| key)
        .collect()
}

fn contains_prefix(keys: &[Vec<u8>], prefix: &[u8]) -> bool {
    keys.iter().any(|key| key.starts_with(prefix))
}

fn contains_exact(keys: &[Vec<u8>], target: &[u8]) -> bool {
    keys.iter().any(|key| key.as_slice() == target)
}

/// Stage a workflow with one started event, one timer, and one outbox row, all
/// written through the public API (so they go through the co-locating routed
/// writes). Returns `(workflow_id, dispatch_key)`.
async fn stage_workflow(store: &HaematiteStore, ordinal: u64) -> (WorkflowId, String) {
    let workflow_id = WorkflowId::new_v4();
    store
        .append(
            WriteToken::recorder(),
            &workflow_id,
            std::slice::from_ref(&started_event(&workflow_id, 1)),
            0,
        )
        .await
        .expect("append started");

    store
        .schedule_timer(&workflow_id, &TimerId::anonymous(ordinal), Utc::now())
        .await
        .expect("schedule timer");

    let row = OutboxRow::pending(
        workflow_id.clone(),
        ordinal,
        String::from("charge"),
        Payload::new(ContentType::Json, b"{}".to_vec()),
        Utc::now() - Duration::hours(1),
    );
    let dispatch_key = row.dispatch_key.clone();
    store
        .append_with_outbox(
            WriteToken::recorder(),
            &workflow_id,
            &[],
            1,
            std::slice::from_ref(&row),
        )
        .await
        .expect("append_with_outbox");

    (workflow_id, dispatch_key)
}

/// Pick two workflow ids whose event streams route to DIFFERENT shards, so the
/// isolation assertion is non-vacuous.
fn two_workflows_on_different_shards(
    database: &haematite::Database,
) -> (WorkflowId, usize, WorkflowId, usize) {
    let first = WorkflowId::new_v4();
    let first_shard = database.shard_for(&event_stream_key(&first));
    loop {
        let second = WorkflowId::new_v4();
        let second_shard = database.shard_for(&event_stream_key(&second));
        if second_shard != first_shard {
            return (first, first_shard, second, second_shard);
        }
    }
}

/// A single workflow's timer + outbox land ONLY on its event stream's shard.
#[tokio::test(flavor = "multi_thread")]
async fn per_workflow_records_land_on_the_workflow_shard() {
    let store =
        HaematiteStore::create_with_shard_count(unique_dir("single"), SHARD_COUNT).expect("create");
    let database = store.event_store().database();

    let (workflow_id, dispatch_key) = stage_workflow(&store, 0).await;
    let owner_shard = database.shard_for(&event_stream_key(&workflow_id));

    let timer_prefix = timer_key_prefix(&workflow_id);
    let outbox = outbox_key(&dispatch_key);

    // The timer key and the outbox key physically appear in the owner shard.
    let owner_keys = shard_kv_keys(database, owner_shard);
    assert!(
        contains_prefix(&owner_keys, &timer_prefix),
        "timer must be co-located on the workflow's shard {owner_shard}"
    );
    assert!(
        contains_exact(&owner_keys, &outbox),
        "outbox row must be co-located on the workflow's shard {owner_shard}"
    );

    // And do NOT appear in any other shard.
    for shard in 0..SHARD_COUNT {
        if shard == owner_shard {
            continue;
        }
        let other = shard_kv_keys(database, shard);
        assert!(
            !contains_prefix(&other, &timer_prefix),
            "timer must NOT appear on non-owner shard {shard}"
        );
        assert!(
            !contains_exact(&other, &outbox),
            "outbox row must NOT appear on non-owner shard {shard}"
        );
    }
}

/// Two workflows on DIFFERENT shards keep their per-workflow records isolated,
/// and the public API still sees everything (fan-out complete despite
/// co-location). Also exercises the `dispatch_key` -> `route_key` derivation in
/// `transition_outbox` via complete/retry.
#[tokio::test(flavor = "multi_thread")]
async fn two_workflows_on_different_shards_stay_isolated() {
    let store = HaematiteStore::create_with_shard_count(unique_dir("isolation"), SHARD_COUNT)
        .expect("create");
    let database = store.event_store().database();

    // Choose two workflows that provably route to different shards.
    let (wf_a, shard_a, wf_b, shard_b) = two_workflows_on_different_shards(database);
    assert_ne!(shard_a, shard_b, "isolation proof must span two shards");

    // Stage both (events + timer + outbox) through the public, co-locating API.
    for (workflow_id, ordinal) in [(&wf_a, 0_u64), (&wf_b, 1_u64)] {
        store
            .append(
                WriteToken::recorder(),
                workflow_id,
                std::slice::from_ref(&started_event(workflow_id, 1)),
                0,
            )
            .await
            .expect("append started");
        store
            .schedule_timer(workflow_id, &TimerId::anonymous(ordinal), Utc::now())
            .await
            .expect("schedule timer");
        let row = OutboxRow::pending(
            workflow_id.clone(),
            ordinal,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            Utc::now() - Duration::hours(1),
        );
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
    }

    let key_a_timer = timer_key_prefix(&wf_a);
    let key_b_timer = timer_key_prefix(&wf_b);
    let key_a_outbox = outbox_key(&OutboxRow::dispatch_key_for(&wf_a, 0));
    let key_b_outbox = outbox_key(&OutboxRow::dispatch_key_for(&wf_b, 1));

    let keys_a = shard_kv_keys(database, shard_a);
    let keys_b = shard_kv_keys(database, shard_b);

    // Each workflow's timer + outbox land on its OWN shard.
    assert!(
        contains_prefix(&keys_a, &key_a_timer) && contains_exact(&keys_a, &key_a_outbox),
        "workflow A's records on its own shard {shard_a}"
    );
    assert!(
        contains_prefix(&keys_b, &key_b_timer) && contains_exact(&keys_b, &key_b_outbox),
        "workflow B's records on its own shard {shard_b}"
    );
    // Neither workflow's records bleed onto the other's shard.
    assert!(
        !contains_prefix(&keys_a, &key_b_timer) && !contains_exact(&keys_a, &key_b_outbox),
        "workflow B's records must NOT be on shard A ({shard_a})"
    );
    assert!(
        !contains_prefix(&keys_b, &key_a_timer) && !contains_exact(&keys_b, &key_a_outbox),
        "workflow A's records must NOT be on shard B ({shard_b})"
    );

    // Public API still sees EVERYTHING across shards despite co-location.
    let expired = store
        .expired_timers(Utc::now() + Duration::seconds(1))
        .await
        .expect("expired timers");
    assert_eq!(expired.len(), 2, "expired_timers fans out to both shards");

    let claimed = store.claim_outbox_rows(10).await.expect("claim");
    assert_eq!(
        claimed.len(),
        2,
        "claim_outbox_rows fans out to both shards"
    );
    assert!(
        claimed
            .iter()
            .all(|row| row.status == OutboxStatus::Claimed)
    );

    // complete/retry route by deriving the workflow id from the dispatch_key,
    // landing the rewrite back on the correct shard (proving the derivation).
    let dispatch_a = OutboxRow::dispatch_key_for(&wf_a, 0);
    let dispatch_b = OutboxRow::dispatch_key_for(&wf_b, 1);
    store
        .complete_outbox_row(&dispatch_a)
        .await
        .expect("complete A");
    store
        .retry_outbox_row(&dispatch_b, 1, Utc::now() - Duration::hours(1))
        .await
        .expect("retry B");

    // A's row is Done (not re-claimable); B's row was retried into the past, so
    // it is claimable again with the bumped attempt — and still co-located on B.
    let reclaimed = store.claim_outbox_rows(10).await.expect("reclaim");
    assert_eq!(reclaimed.len(), 1, "only B's retried row is re-claimable");
    assert_eq!(reclaimed[0].dispatch_key, dispatch_b);
    assert_eq!(reclaimed[0].attempt, 1);

    let keys_b_after = shard_kv_keys(database, shard_b);
    assert!(
        contains_exact(&keys_b_after, &key_b_outbox),
        "B's retried row stays co-located on shard {shard_b}"
    );
}
