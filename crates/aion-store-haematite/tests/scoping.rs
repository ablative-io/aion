//! Owned-shard ENUMERATION scoping for `HaematiteStore` (AA-4-3b).
//!
//! AA-4-3a co-located every per-workflow KV record (events + timers + outbox) on
//! the workflow's event-stream shard. AA-4-3b lets the store be told it owns only
//! a SUBSET of shards and scopes all per-workflow enumeration to those shards, so
//! a node owning shards `{0}` enumerates ONLY the workflows / timers / outbox rows
//! that physically live on shard 0. The engine stays backend-agnostic: it calls
//! the same trait methods and transparently sees only its shards' work.
//!
//! This test builds a `shard_count == 3` store, stages >=6 workflows that span
//! >=2 shards (non-vacuous), and proves:
//!
//! * DEFAULT (own all): `list_active` / `list_workflow_ids` / `expired_timers` /
//!   `claim_outbox_rows` see EVERY workflow across all shards.
//! * `set_owned_shards([0])`: enumeration collapses to a PROPER subset — exactly
//!   the shard-0 group, with the excluded shards' workflows/timers/rows ABSENT.
//! * `own_all_shards()` restores full enumeration.
//! * `list_packages` is UNAFFECTED by `set_owned_shards` (packages/routes are
//!   cluster-wide node-local, not per-workflow co-located).
//!
//! Workflow ids are compared by their string form (sorted `Vec<String>`) because
//! `WorkflowId` is not `Ord` (matching how `multishard.rs` compares id lists).

// Test code asserts via expect/unwrap exactly like the crate's other test
// binaries (conformance, multishard, colocation, distributed_failover); allow
// the restriction lints here the same way.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::similar_names,
    clippy::too_many_lines
)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_core::{
    ContentType, Event, EventEnvelope, PackageVersion, Payload, RunId, TimerId, WorkflowId,
};
use aion_store::{
    OutboxRow, OutboxStatus, OutboxStore, PackageRecord, PackageStore, ReadableEventStore,
    WritableEventStore, WriteToken,
};
use aion_store_haematite::HaematiteStore;
use chrono::{Duration, Utc};

const SHARD_COUNT: usize = 3;
const WORKFLOWS: usize = 6;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "aion-store-haematite-scoping-{name}-{}-{nanos}-{counter}",
        std::process::id()
    ))
}

/// Reproduce the adapter's PRIVATE `keyspace::event_stream_key` encoding (`E`
/// tag byte followed by the raw 16-byte UUID) so the test can ask haematite
/// which shard a workflow's event stream routes to, WITHOUT touching production
/// key encoding. This is exactly the route key per-workflow records co-locate on.
fn event_stream_key(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 16);
    key.push(b'E');
    key.extend_from_slice(workflow_id.as_uuid().as_bytes());
    key
}

/// Sorted string form of a workflow-id collection. `WorkflowId` is not `Ord`, so
/// id sets are compared by their string representation (as `multishard.rs` does).
fn id_strings<'a>(ids: impl IntoIterator<Item = &'a WorkflowId>) -> Vec<String> {
    let mut out: Vec<String> = ids.into_iter().map(ToString::to_string).collect();
    out.sort();
    out
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

/// Stage one workflow with a started event, a timer (fires in the past), and a
/// claimable (Pending, past `visible_after`) outbox row — all through the public
/// API, so they go through the co-locating routed writes. Returns the workflow id
/// and its outbox `dispatch_key`.
async fn stage_workflow(store: &HaematiteStore, ordinal: u64) -> (WorkflowId, String) {
    let workflow_id = WorkflowId::new_v4();
    let past = Utc::now() - Duration::hours(1);

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
        .schedule_timer(&workflow_id, &TimerId::anonymous(ordinal), past)
        .await
        .expect("schedule timer");

    let row = OutboxRow::pending(
        workflow_id.clone(),
        ordinal,
        String::from("charge"),
        Payload::new(ContentType::Json, b"{}".to_vec()),
        past,
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

/// Default enumeration (own ALL shards) sees every workflow / timer / outbox row
/// across all shards; `set_owned_shards([0])` collapses to exactly the shard-0
/// subset (a PROPER subset); `own_all_shards()` restores the full view. Packages
/// stay cluster-wide and are unaffected.
#[tokio::test(flavor = "multi_thread")]
async fn enumeration_scopes_to_owned_shards() {
    let store =
        HaematiteStore::create_with_shard_count(unique_dir("scope"), SHARD_COUNT).expect("create");
    let database = store.event_store().database();

    // Stage the workflows and group each by the shard its event stream routes to,
    // reproducing the `E||uuid` route bytes locally (keyspace is pub(crate)).
    let mut all_ids: Vec<WorkflowId> = Vec::new();
    let mut shard0_ids: Vec<WorkflowId> = Vec::new();
    let mut shard0_dispatch: BTreeSet<String> = BTreeSet::new();
    let mut non_shard0_ids: Vec<WorkflowId> = Vec::new();
    let mut shards_seen: BTreeSet<usize> = BTreeSet::new();

    for ordinal in 0..WORKFLOWS as u64 {
        let (workflow_id, dispatch_key) = stage_workflow(&store, ordinal).await;
        let shard = database.shard_for(&event_stream_key(&workflow_id));
        shards_seen.insert(shard);
        all_ids.push(workflow_id.clone());
        if shard == 0 {
            shard0_ids.push(workflow_id);
            shard0_dispatch.insert(dispatch_key);
        } else {
            non_shard0_ids.push(workflow_id);
        }
    }

    // Non-vacuous: the workflows span at least two shards.
    assert!(
        shards_seen.len() >= 2,
        "workflows must span >=2 shards (saw {shards_seen:?}); test would be vacuous otherwise"
    );
    // Shard 0 must hold SOME (but not all) workflows for the proper-subset asserts.
    assert!(
        !shard0_ids.is_empty(),
        "expected at least one workflow on shard 0"
    );
    assert!(
        !non_shard0_ids.is_empty(),
        "expected at least one workflow off shard 0"
    );

    let all_strings = id_strings(&all_ids);
    let shard0_strings = id_strings(&shard0_ids);

    // Stage two packages to prove they are unaffected by scoping below.
    for index in 0..2u32 {
        store
            .put_package(PackageRecord {
                workflow_type: format!("type-{index}"),
                content_hash: format!("{index}").repeat(64),
                archive: vec![u8::try_from(index).unwrap_or(0)],
                deployed_at: Utc::now(),
            })
            .await
            .expect("put package");
    }

    // --- DEFAULT (own all shards): everything is visible ----------------------
    assert_eq!(store.owned_shards(), None, "default owns ALL shards");

    let active_all = store.list_active().await.expect("list_active all");
    assert_eq!(
        id_strings(&active_all),
        all_strings,
        "own-all list_active sees every workflow"
    );

    let ids_all = store
        .list_workflow_ids()
        .await
        .expect("list_workflow_ids all");
    assert_eq!(
        id_strings(&ids_all),
        all_strings,
        "own-all list_workflow_ids sees every workflow"
    );

    let timers_all = store.expired_timers(Utc::now()).await.expect("timers all");
    assert_eq!(
        timers_all.len(),
        WORKFLOWS,
        "own-all expired_timers returns every timer"
    );

    let packages_all = store.list_packages().await.expect("packages all");
    assert_eq!(
        packages_all.len(),
        2,
        "own-all list_packages sees both packages"
    );

    // --- set_owned_shards([0]): only the shard-0 subset is visible ------------
    store.set_owned_shards([0]);
    assert_eq!(
        store.owned_shards(),
        Some(vec![0]),
        "owned_shards snapshots the set"
    );

    let active_scoped = store.list_active().await.expect("list_active scoped");
    // PROPER subset: strictly smaller AND exactly the shard-0 group.
    assert!(
        active_scoped.len() < active_all.len(),
        "scoped list_active is a PROPER subset ({} < {})",
        active_scoped.len(),
        active_all.len()
    );
    assert_eq!(
        id_strings(&active_scoped),
        shard0_strings,
        "scoped list_active returns ONLY the shard-0 workflows"
    );
    // The excluded shards' workflows are ABSENT.
    let scoped_active_strings = id_strings(&active_scoped);
    for excluded in &non_shard0_ids {
        assert!(
            !scoped_active_strings.contains(&excluded.to_string()),
            "off-shard-0 workflow {excluded} must be absent from scoped list_active"
        );
    }

    let ids_scoped = store
        .list_workflow_ids()
        .await
        .expect("list_workflow_ids scoped");
    assert_eq!(
        id_strings(&ids_scoped),
        shard0_strings,
        "scoped list_workflow_ids returns ONLY the shard-0 workflows"
    );

    let timers_scoped = store
        .expired_timers(Utc::now())
        .await
        .expect("timers scoped");
    let timer_id_refs: Vec<&WorkflowId> = timers_scoped
        .iter()
        .map(|entry| &entry.workflow_id)
        .collect();
    assert_eq!(
        id_strings(timer_id_refs),
        shard0_strings,
        "scoped expired_timers returns ONLY shard-0 timers"
    );
    assert_eq!(
        timers_scoped.len(),
        shard0_ids.len(),
        "scoped timer count equals shard-0 workflow count"
    );

    // list_packages is UNAFFECTED by scoping (cluster-wide node-local records).
    let packages_scoped = store.list_packages().await.expect("packages scoped");
    assert_eq!(
        packages_scoped.len(),
        2,
        "set_owned_shards must NOT scope list_packages"
    );

    // claim_outbox_rows claims ONLY shard-0 rows (and mutates state, so this is
    // the last claim assertion on this store handle).
    let claimed_scoped = store.claim_outbox_rows(100).await.expect("claim scoped");
    assert!(
        claimed_scoped
            .iter()
            .all(|row| row.status == OutboxStatus::Claimed),
        "claimed rows are marked Claimed"
    );
    let claimed_dispatch: BTreeSet<String> = claimed_scoped
        .iter()
        .map(|row| row.dispatch_key.clone())
        .collect();
    assert_eq!(
        claimed_dispatch, shard0_dispatch,
        "scoped claim_outbox_rows claims ONLY the shard-0 outbox rows"
    );

    // --- own_all_shards(): full enumeration restored --------------------------
    store.own_all_shards();
    assert_eq!(store.owned_shards(), None, "own_all_shards reverts to ALL");

    let active_restored = store.list_active().await.expect("list_active restored");
    assert_eq!(
        id_strings(&active_restored),
        all_strings,
        "own_all_shards restores the full workflow enumeration"
    );
    let timers_restored = store
        .expired_timers(Utc::now())
        .await
        .expect("timers restored");
    assert_eq!(
        timers_restored.len(),
        WORKFLOWS,
        "own_all_shards restores the full timer enumeration"
    );
}

/// `claim_outbox_rows` under own-all claims EVERY shard's row (the default-path
/// baseline for the scoped claim above). Uses a FRESH store so the claim mutation
/// does not interact with the scoped test's state.
#[tokio::test(flavor = "multi_thread")]
async fn default_claim_spans_all_shards() {
    let store = HaematiteStore::create_with_shard_count(unique_dir("claim-all"), SHARD_COUNT)
        .expect("create");
    let database = store.event_store().database();

    let mut all_dispatch: BTreeSet<String> = BTreeSet::new();
    let mut shards_seen: BTreeSet<usize> = BTreeSet::new();
    for ordinal in 0..WORKFLOWS as u64 {
        let (workflow_id, dispatch_key) = stage_workflow(&store, ordinal).await;
        shards_seen.insert(database.shard_for(&event_stream_key(&workflow_id)));
        all_dispatch.insert(dispatch_key);
    }
    assert!(shards_seen.len() >= 2, "workflows must span >=2 shards");

    let claimed = store.claim_outbox_rows(100).await.expect("claim all");
    let claimed_dispatch: BTreeSet<String> =
        claimed.iter().map(|row| row.dispatch_key.clone()).collect();
    assert_eq!(
        claimed_dispatch, all_dispatch,
        "own-all claim_outbox_rows claims rows across ALL shards"
    );
}
