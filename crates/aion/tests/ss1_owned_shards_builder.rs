//! SS-1: owned-shard scoping driven through the PRODUCTION `EngineBuilder`.
//!
//! `scoping.rs` (in `aion-store-haematite`) proves owned-shard ENUMERATION via
//! the store's inherent `set_owned_shards` test back door. SS-1 wires that
//! capability into the engine boot path: `EngineBuilder::owned_shards(set)`
//! applies the scope to the (possibly decorator-wrapped) store BEFORE startup
//! recovery, so a node recovers and enumerates ONLY its shards.
//!
//! This test does the equivalent of `scoping.rs` but from the builder, never the
//! back door:
//!
//! * Stage >=6 workflows on a `shard_count == 3` haematite store so they span
//!   >=2 shards (non-vacuous).
//! * Build engine A through `EngineBuilder::owned_shards([0])`; assert its store
//!   enumerates ONLY the shard-0 workflows (a PROPER subset).
//! * Build engine B through `EngineBuilder::owned_shards(<the other shards>)`;
//!   assert its store enumerates ONLY the off-shard-0 workflows.
//! * The two owned sets are DISJOINT and union to every staged workflow — proof
//!   that the builder, not a test back door, scoped each engine to its shards.
//!
//! Coordinator bootstrap is disabled on both engines (these nodes are not the
//! coordinator-shard owner), exactly as a non-owner does in the active-active
//! showcase, so the only active workflows are the staged ones.

use std::collections::BTreeSet;
use std::error::Error;
use std::sync::Arc;

use aion::EngineBuilder;
use aion_core::{ContentType, Event, EventEnvelope, PackageVersion, Payload, RunId, WorkflowId};
use aion_store::{EventStore, WritableEventStore, WriteToken};
use aion_store_haematite::HaematiteStore;
use chrono::Utc;

type TestResult = Result<(), Box<dyn Error>>;

const SHARD_COUNT: usize = 3;
const WORKFLOWS: usize = 6;

fn unique_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "aion-ss1-owned-shards-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// Reproduce the adapter's PRIVATE `event_stream_key` encoding (`E` tag byte +
/// the raw 16-byte UUID) so the test can ask haematite which shard a workflow's
/// event stream routes to, without touching production key encoding.
fn event_stream_key(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 16);
    key.push(b'E');
    key.extend_from_slice(workflow_id.as_uuid().as_bytes());
    key
}

fn started_event(workflow_id: &WorkflowId) -> Event {
    Event::WorkflowStarted {
        envelope: EventEnvelope {
            seq: 1,
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

/// Deterministically choose `count` workflow ids that span >=2 shards with shard 0
/// holding at least one and at least one OFF shard 0. Random `new_v4` ids leave
/// shard 0 empty ~(2/3)^6 of the time at `WORKFLOWS == 6`, which previously flaked
/// the "at least one workflow on shard 0" gate under CPU-starved parallel load.
/// Rejection-sampling over `shard_of` (a total function of the id) always
/// converges and yields a set that satisfies the spanning gates by construction.
/// Mirrors `pick_spanning_ids` in `aion-store-haematite`'s `scoping.rs`.
fn pick_spanning_ids(count: usize, shard_of: impl Fn(&WorkflowId) -> usize) -> Vec<WorkflowId> {
    let mut on_shard0: Vec<WorkflowId> = Vec::new();
    let mut off_shard0: Vec<WorkflowId> = Vec::new();
    while on_shard0.is_empty()
        || off_shard0.is_empty()
        || on_shard0.len() + off_shard0.len() < count
    {
        let id = WorkflowId::new_v4();
        if shard_of(&id) == 0 {
            on_shard0.push(id);
        } else {
            off_shard0.push(id);
        }
    }
    // Guarantee >=1 on shard 0 and >=1 off it, then fill to `count` from either bucket.
    let mut chosen = Vec::with_capacity(count);
    chosen.push(on_shard0.remove(0));
    chosen.push(off_shard0.remove(0));
    for id in on_shard0.into_iter().chain(off_shard0) {
        if chosen.len() == count {
            break;
        }
        chosen.push(id);
    }
    chosen
}

/// Sorted string form of a workflow-id collection. `WorkflowId` is not `Ord`, so
/// id sets are compared by their string representation (as `scoping.rs` does).
fn id_strings<'a>(ids: impl IntoIterator<Item = &'a WorkflowId>) -> Vec<String> {
    let mut out: Vec<String> = ids.into_iter().map(ToString::to_string).collect();
    out.sort();
    out
}

/// Build an engine over `store` scoped to `owned` shards via the PRODUCTION
/// builder, with coordinator bootstrap off (this node is not the coordinator
/// owner), then return the set of active workflow ids its scoped store sees.
async fn active_ids_scoped_to(
    store: Arc<dyn EventStore>,
    owned: &[usize],
) -> Result<Vec<WorkflowId>, Box<dyn Error>> {
    let engine = EngineBuilder::new()
        .store_arc(store)
        .in_memory_visibility()
        .scheduler_threads(1)
        .bootstrap_schedule_coordinator(false)
        .owned_shards(owned.iter().copied())
        .build()
        .await?;
    let active = engine.store().list_active().await?;
    engine.shutdown()?;
    Ok(active)
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_owned_shards_scopes_recovery_to_disjoint_sets() -> TestResult {
    let dir = unique_dir();
    let store = HaematiteStore::create_with_shard_count(&dir, SHARD_COUNT)?;
    let database = store.event_store().database();

    // Stage WORKFLOWS workflows through the public append path (routed writes
    // co-locate each on its event-stream shard), grouping by shard.
    let mut all_ids: Vec<WorkflowId> = Vec::new();
    let mut shard0_ids: Vec<WorkflowId> = Vec::new();
    let mut other_ids: Vec<WorkflowId> = Vec::new();
    let mut shards_seen: BTreeSet<usize> = BTreeSet::new();
    // Deterministic ids chosen to GUARANTEE the shard coverage the gates below
    // assert (>=1 on shard 0, >=1 off it), instead of relying on the random shard
    // distribution of `new_v4` ids (the source of flake #83).
    let chosen_ids = pick_spanning_ids(WORKFLOWS, |id| database.shard_for(&event_stream_key(id)));
    for workflow_id in chosen_ids {
        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&started_event(&workflow_id)),
                0,
            )
            .await?;
        let shard = database.shard_for(&event_stream_key(&workflow_id));
        shards_seen.insert(shard);
        all_ids.push(workflow_id.clone());
        if shard == 0 {
            shard0_ids.push(workflow_id);
        } else {
            other_ids.push(workflow_id);
        }
    }

    // Non-vacuous: the workflows span at least two shards, and both the shard-0
    // group and its complement are non-empty (so each owned set is meaningful).
    assert!(
        shards_seen.len() >= 2,
        "workflows must span >=2 shards (saw {shards_seen:?}); test would be vacuous otherwise"
    );
    assert!(
        !shard0_ids.is_empty(),
        "expected at least one workflow on shard 0"
    );
    assert!(
        !other_ids.is_empty(),
        "expected at least one workflow off shard 0"
    );

    let all_strings = id_strings(&all_ids);
    let shard0_strings = id_strings(&shard0_ids);
    let other_strings = id_strings(&other_ids);

    // The two owned sets are DISJOINT and cover every staged workflow.
    assert_eq!(
        shard0_strings.len() + other_strings.len(),
        all_strings.len(),
        "shard-0 and complement owned sets must be disjoint and cover all workflows"
    );

    // Drop the staging handle so the engines own the on-disk store exclusively.
    drop(store);

    // --- Engine A: owns ONLY shard 0 -----------------------------------------
    let store_first: Arc<dyn EventStore> = Arc::new(HaematiteStore::open(&dir)?);
    let active_first = active_ids_scoped_to(store_first, &[0]).await?;
    let first_view = id_strings(&active_first);
    assert_eq!(
        first_view, shard0_strings,
        "engine built with owned_shards([0]) enumerates ONLY the shard-0 workflows"
    );
    assert!(
        active_first.len() < all_ids.len(),
        "scoped engine A sees a PROPER subset ({} < {})",
        active_first.len(),
        all_ids.len()
    );

    // --- Engine B: owns the complement of shard 0 ----------------------------
    let other_shards: Vec<usize> = (1..SHARD_COUNT).collect();
    let store_second: Arc<dyn EventStore> = Arc::new(HaematiteStore::open(&dir)?);
    let active_second = active_ids_scoped_to(store_second, &other_shards).await?;
    let second_view = id_strings(&active_second);
    assert_eq!(
        second_view, other_strings,
        "engine built with owned_shards(complement) enumerates ONLY the off-shard-0 workflows"
    );

    // The two scoped engines saw DISJOINT workflow sets whose union is the whole.
    let first_set: BTreeSet<String> = first_view.into_iter().collect();
    let second_set: BTreeSet<String> = second_view.into_iter().collect();
    assert!(
        first_set.is_disjoint(&second_set),
        "the two engines' owned-shard views must be disjoint"
    );
    let union: BTreeSet<String> = first_set.union(&second_set).cloned().collect();
    assert_eq!(
        union.into_iter().collect::<Vec<_>>(),
        all_strings,
        "the union of the two engines' views is every staged workflow"
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
