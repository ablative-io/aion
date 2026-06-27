//! SS-2: shard ELECTION driven through the PRODUCTION `EngineBuilder`.
//!
//! SS-1 (`ss1_owned_shards_builder.rs`) wired owned-shard SCOPING into the boot
//! path over a SINGLE-NODE store (no election). SS-2 adds the election step: for
//! each owned shard, `EngineBuilder::build` runs `acquire_shard_and_serve` (via
//! the store's `acquire_owned_shards` seam, off the tokio runtime) BEFORE
//! recovery, so the node is the fenced live owner of its shards and recovery sees
//! the union-merged committed history.
//!
//! This test drives that from the PRODUCTION builder, not the showcase harness:
//!
//! * Build a "cluster of one" DISTRIBUTED haematite store
//!   (`open_or_create_distributed` with no peers, denominator 1) so the engine
//!   boot path exercises the REAL election path (`with_distribution` →
//!   `acquire_shard_and_serve`), not the single-node no-op.
//! * Stage a workflow on shard 0 through the replicated append path.
//! * Build the engine through `EngineBuilder::owned_shards([0])`. The builder
//!   elects shard 0 (becomes the live fenced owner) BEFORE recovery, then
//!   recovers only shard-0 workflows and bootstraps the coordinator iff it owns
//!   the coordinator's shard.
//! * Assert: the staged workflow recovered (election + `become_live` made it
//!   locally present); exactly ONE coordinator `WorkflowStarted` exists when this
//!   node owns the coordinator shard, and NONE when it does not (the AA-4-4 gate
//!   fed from real ownership).
//!
//! ## Why one long-lived runtime + `block_on` (not `#[tokio::test]`)
//!
//! Identical to the active-active showcase: binding the replication endpoint and
//! running the blocking election refuse to run from a thread with an entered
//! tokio runtime, while the engine builder is async and captures
//! `Handle::current()`. ONE long-lived runtime drives the async engine build
//! through `block_on`; the off-runtime election runs on a bare thread inside the
//! store seam. Single-node-cluster election self-quorums, so this is non-flaky.
//!
//! Multi-node e2e election from the production builder is DEFERRED to an SS-2
//! follow-up (it needs the cluster supervisor / membership-loss trigger of SS-5);
//! this lands the single-node-cluster election path cleanly.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use aion::{EngineBuilder, schedule_coordinator_workflow_id};
use aion_core::{ContentType, Event, EventEnvelope, PackageVersion, Payload, RunId, WorkflowId};
use aion_store::{EventStore, ReadableEventStore, WriteToken};
use aion_store_haematite::{ClusterBootstrap, HaematiteStore};

type TestResult = Result<(), Box<dyn Error>>;

const SHARD_COUNT: usize = 3;
const OP_TIMEOUT: Duration = Duration::from_secs(5);

fn unique_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "aion-ss2-shard-election-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

fn started_event(workflow_id: &WorkflowId) -> Event {
    Event::WorkflowStarted {
        envelope: EventEnvelope {
            seq: 1,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: String::from("checkout"),
        input: Payload::new(ContentType::Json, b"{}".to_vec()),
        run_id: RunId::new_v4(),
        parent_run_id: None,
        package_version: PackageVersion::new("a".repeat(64)),
    }
}

/// A cluster-of-one bootstrap: this node, no peers, denominator 1.
fn cluster_of_one(bind_address: std::net::SocketAddr) -> ClusterBootstrap {
    ClusterBootstrap {
        node_id: String::from("ss2-node@127.0.0.1"),
        bind_address,
        members: vec![String::from("ss2-node@127.0.0.1")],
        peers: Vec::new(),
        timeout: OP_TIMEOUT,
    }
}

/// Mint a `WorkflowId` whose event stream routes to `shard` over `shard_count`.
fn workflow_id_for_shard(store: &HaematiteStore, shard: usize) -> WorkflowId {
    loop {
        let candidate = WorkflowId::new_v4();
        if store.shard_for_workflow(&candidate) == shard {
            return candidate;
        }
    }
}

#[test]
fn builder_elects_owned_shard_before_recovery_in_a_cluster_of_one() -> TestResult {
    // ONE long-lived runtime drives the async engine build; the off-runtime
    // election runs on a bare thread inside the store seam BETWEEN block_ons.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let dir = unique_dir();

    // Build a DISTRIBUTED cluster-of-one store on a bare thread (endpoint bind
    // refuses an entered runtime; the constructor steps off-runtime internally).
    let (store, _responder) = HaematiteStore::open_or_create_distributed(
        &dir,
        SHARD_COUNT,
        cluster_of_one("127.0.0.1:0".parse()?),
    )?;

    // The store must NOT be the live owner yet — that is the builder's job.
    // Take ownership of shard 0 ONCE here only to stage the workflow through the
    // replicated append path, then release the engine to re-elect on build.
    store.acquire_owned_shards(&[0])?;
    store.set_owned_shards([0]);

    // Stage a workflow that routes to shard 0 (replicated append to the quorum
    // of one), so recovery has something to find ONLY if election + become_live
    // made it locally present.
    let workflow_id = workflow_id_for_shard(&store, 0);

    // Which shard owns the coordinator? Only its owner bootstraps it. Compute it
    // from the concrete store BEFORE type-erasing it into the engine.
    let coordinator_id = schedule_coordinator_workflow_id();
    let owns_coordinator = store.shard_for_workflow(&coordinator_id) == 0;

    let store_arc: Arc<dyn EventStore> = Arc::new(store);
    runtime.block_on(async {
        store_arc
            .append(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&started_event(&workflow_id)),
                0,
            )
            .await
    })?;

    // Build the engine through the PRODUCTION builder. `owned_shards([0])` drives
    // BOTH the scoping and the SS-2 election (acquire_shard_and_serve) BEFORE
    // recovery. Coordinator bootstrap is gated on real ownership.
    let engine = runtime.block_on(
        EngineBuilder::new()
            .store_arc(Arc::clone(&store_arc))
            .in_memory_visibility()
            .scheduler_threads(1)
            .bootstrap_schedule_coordinator(owns_coordinator)
            .owned_shards([0])
            .build(),
    )?;

    // The staged workflow recovered: election + become_live made shard 0's
    // committed history locally present before recovery enumerated it.
    let active = runtime.block_on(engine.store().list_active())?;
    let active_strings: Vec<String> = active.iter().map(ToString::to_string).collect();
    assert!(
        active_strings.contains(&workflow_id.to_string()),
        "the elected owner recovered its shard-0 workflow (active = {active_strings:?})"
    );

    // Coordinator bootstrap reflects real ownership: exactly one WorkflowStarted
    // when this node owns the coordinator's shard, none otherwise.
    let coordinator_history = runtime.block_on(engine.store().read_history(&coordinator_id))?;
    let coordinator_starts = coordinator_history
        .iter()
        .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
        .count();
    if owns_coordinator {
        assert_eq!(
            coordinator_starts, 1,
            "the coordinator-shard owner bootstraps exactly one coordinator"
        );
    } else {
        assert_eq!(
            coordinator_starts, 0,
            "a non-owner of the coordinator shard must not seed the coordinator stream"
        );
    }

    engine.shutdown()?;
    drop(store_arc);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
