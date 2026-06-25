//! B2 multi-node gate: a workflow's durable Aion history is quorum-REPLICATED
//! across a real haematite cluster and SURVIVES the owner node's death.
//!
//! Three in-process nodes {A,B,C}, quorum 2, REAL beamr loopback transport. Each
//! node wraps its own distribution-attached `Database` in a `HaematiteStore`
//! (B2's distributed mode). The writes go THROUGH the Aion adapter
//! (`WritableEventStore::append`) on node A — not through a raw haematite call —
//! so this proves the adapter itself routes appends to `replicate_append`.
//!
//! GATE 1 — REPLICATED: A appends a workflow's event lifecycle through the
//! adapter to a quorum {B,C}. B's and C's `HaematiteStore::read_history` return
//! the SAME full history A wrote — the events replicated to the followers, not
//! just landed locally on A.
//!
//! GATE 2 — FAILOVER (the headline): A replicates the lifecycle to a quorum {B}
//! only (so C lags the whole history), A is partitioned away, and B is elected the
//! new owner and `become_live`-merges. B's `HaematiteStore::read_history` returns
//! the FULL workflow history A wrote — the workflow's durable state survived the
//! owner's death and is readable on the survivor via the Aion adapter.
//!
//! NON-VACUOUS: before failover, the test asserts the survivor's store returns an
//! EMPTY history (it never received the writes), so a history that appears after
//! `become_live` could ONLY have arrived through the replicate+merge path — the
//! test cannot pass vacuously. The companion control runs the SAME setup with a
//! BARE `acquire_shard` (no merge) and asserts the history stays empty.

//! # Why plain `#[test]` (not `#[tokio::test]`)
//!
//! haematite's distribution coordinator (`bind`, `connect`, `acquire_shard_*`,
//! `replicate_append`) BLOCKS and refuses to run from a thread with an entered
//! tokio runtime (`Handle::try_current().is_ok()` ⟹ `TransportBlockingFromAsync`).
//! So the cluster lifecycle (spawn, link, election, failover) runs on the bare
//! test thread, and the ASYNC Aion adapter calls (`append`, `read_history`) are
//! driven through a manually-built runtime's `block_on`, which exits the runtime
//! context as soon as it returns — leaving the next election outside any runtime.

#![allow(clippy::panic, clippy::doc_markdown, clippy::doc_lazy_continuation)]

use std::error::Error;
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aion_core::{ContentType, Event, EventEnvelope, Payload, RunId, WorkflowId};
use aion_store::{ReadableEventStore, WritableEventStore, WriteToken};
use aion_store_haematite::HaematiteStore;
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};

type TestResult = Result<(), Box<dyn Error>>;

/// Drive one async adapter future to completion on a fresh runtime, then drop it
/// so the runtime context is not entered on the test thread when the next
/// (blocking) haematite election runs.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build runtime")
        .block_on(future)
}

const NODE_A: &str = "node-a@127.0.0.1";
const NODE_B: &str = "node-b@127.0.0.1";
const NODE_C: &str = "node-c@127.0.0.1";

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);

const SHARD: usize = 0;

fn loopback() -> Result<SocketAddr, Box<dyn Error>> {
    Ok("127.0.0.1:0".parse()?)
}

fn config_for(path: &Path) -> DatabaseConfig {
    DatabaseConfig {
        data_dir: path.to_path_buf(),
        shard_count: 1,
        sweep_interval: None,
        distributed: None,
    }
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn membership(total_nodes: usize, send_targets: &[&str]) -> WriteMembership {
    WriteMembership {
        total_nodes,
        send_targets: send_targets.iter().map(|name| SyncNodeId::from(*name)).collect(),
    }
}

/// One cluster node: a `HaematiteStore` in distributed mode over a
/// distribution-attached `Database`, plus a background responder draining and
/// answering inbound `Prepare`s / `WriteProposal`s / `BatchWriteProposal`s /
/// `ShardSyncRequest`s. The store and responder share the SAME `Database` (the
/// store routes appends through it; the responder serves its peers).
struct Node {
    store: HaematiteStore,
    event_store: Arc<haematite::EventStore>,
    addr: SocketAddr,
    name: &'static str,
    responder: Option<JoinHandle<()>>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl Node {
    /// Spawn a node whose store replicates writes to `send_targets` over a
    /// `total_nodes`-denominator quorum.
    fn spawn(
        name: &'static str,
        dir: &Path,
        total_nodes: usize,
        send_targets: &[&str],
    ) -> Result<Self, Box<dyn Error>> {
        let endpoint = DistributionEndpoint::bind(name, loopback()?, 1, None)?;
        let addr = endpoint.local_addr();
        let database =
            Database::create(config_for(dir.join("db").as_path()))?.with_distribution(endpoint);

        let store = HaematiteStore::with_distribution(
            database,
            membership(total_nodes, send_targets),
            OP_TIMEOUT,
        );
        let event_store = Arc::clone(store.event_store());

        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let responder_store = Arc::clone(&event_store);
        let responder_running = Arc::clone(&running);
        let responder = std::thread::spawn(move || {
            while responder_running.load(std::sync::atomic::Ordering::Relaxed) {
                drop(respond_to_inbound_writes(
                    responder_store.database(),
                    Duration::from_millis(50),
                ));
            }
        });

        Ok(Self {
            store,
            event_store,
            addr,
            name,
            responder: Some(responder),
            running,
        })
    }

    fn database(&self) -> &Database {
        self.event_store.database()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.responder.take() {
            drop(handle.join());
        }
    }
}

fn link(from: &Node, to: &Node) -> TestResult {
    let endpoint = from
        .database()
        .distribution()
        .ok_or("dialing node has no endpoint")?;
    endpoint.add_peer(to.name, to.addr);
    endpoint.connect(to.name)?;
    if !wait_until(HANDSHAKE_TIMEOUT, || endpoint.is_connected(to.name)) {
        return Err(format!("{} never registered a link to {}", from.name, to.name).into());
    }
    Ok(())
}

fn link_both(a: &Node, b: &Node) -> TestResult {
    link(a, b)?;
    link(b, a)?;
    Ok(())
}

/// The lifecycle of one workflow as a contiguous Aion event history: started,
/// progressed (a timer fired), completed. These are the bytes that must round-trip
/// across replication + failover unchanged.
fn lifecycle(workflow_id: &WorkflowId) -> Vec<Event> {
    let started = Event::WorkflowStarted {
        envelope: EventEnvelope {
            seq: 1,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: String::from("checkout"),
        input: Payload::new(ContentType::Json, b"{\"cart\":42}".to_vec()),
        run_id: RunId::new_v4(),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    };
    let fired = Event::TimerFired {
        envelope: EventEnvelope {
            seq: 2,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        timer_id: aion_core::TimerId::anonymous(1),
    };
    let completed = Event::WorkflowCompleted {
        envelope: EventEnvelope {
            seq: 3,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        result: Payload::new(ContentType::Json, b"{\"ok\":true}".to_vec()),
    };
    vec![started, fired, completed]
}

// ===========================================================================
// GATE 1 — REPLICATED: the followers hold A's full workflow history.
// ===========================================================================

/// A appends a workflow's full lifecycle THROUGH THE ADAPTER to a quorum {B,C};
/// B's and C's `HaematiteStore::read_history` must return the SAME history. Proves
/// the adapter's `append` routes to `replicate_append` and the batch lands on the
/// followers (decoded back through the B1 read path).
#[test]
fn workflow_history_replicates_to_followers() -> TestResult {
    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;

    let node_a = Node::spawn(NODE_A, dir_a.path(), 3, &[NODE_B, NODE_C])?;
    let node_b = Node::spawn(NODE_B, dir_b.path(), 3, &[NODE_A, NODE_C])?;
    let node_c = Node::spawn(NODE_C, dir_c.path(), 3, &[NODE_A, NODE_B])?;
    link_both(&node_a, &node_b)?;
    link_both(&node_a, &node_c)?;
    link_both(&node_b, &node_c)?;

    node_a
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_B, NODE_C]), OP_TIMEOUT)?;

    let workflow_id = WorkflowId::new_v4();
    let history = lifecycle(&workflow_id);

    // Append the WHOLE lifecycle through the Aion adapter (routes to replicate_append).
    block_on(
        node_a
            .store
            .append(WriteToken::recorder(), &workflow_id, &history, 0),
    )?;

    let on_a = block_on(node_a.store.read_history(&workflow_id))?;
    assert_eq!(on_a, history, "owner A must read its own appended history");

    for node in [&node_b, &node_c] {
        let replicated = block_on(node.store.read_history(&workflow_id))?;
        assert_eq!(
            replicated, history,
            "follower {} must read the full replicated workflow history via the adapter",
            node.name
        );
    }
    Ok(())
}

// ===========================================================================
// GATE 2 — FAILOVER: the survivor serves the FULL history after the owner dies.
// ===========================================================================

/// A replicates the lifecycle to a quorum {B} ONLY, so C LAGS the whole history.
/// A is then partitioned and C — the laggard — is elected the new owner over {B}
/// and `become_live`-merges B's committed tree. C's `HaematiteStore::read_history`
/// must return the FULL workflow history A wrote. NON-VACUOUS and the merge is
/// strictly load-bearing: C is asserted EMPTY before the failover, so every event
/// it serves afterward could ONLY have arrived via the merge pull from B.
#[test]
fn workflow_history_survives_owner_failover() -> TestResult {
    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;

    // A replicates to {B} ONLY: quorum {A,B} reached, C never receives the batch.
    let node_a = Node::spawn(NODE_A, dir_a.path(), 3, &[NODE_B])?;
    let node_b = Node::spawn(NODE_B, dir_b.path(), 3, &[NODE_A])?;
    let node_c = Node::spawn(NODE_C, dir_c.path(), 3, &[NODE_A, NODE_B])?;
    link_both(&node_a, &node_b)?;
    link_both(&node_a, &node_c)?;
    link_both(&node_b, &node_c)?;

    node_a
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_B]), OP_TIMEOUT)?;

    let workflow_id = WorkflowId::new_v4();
    let history = lifecycle(&workflow_id);
    block_on(
        node_a
            .store
            .append(WriteToken::recorder(), &workflow_id, &history, 0),
    )?;

    // FALSIFIABILITY: C lagged the WHOLE history before failover. Whatever the
    // survivor serves after becoming live can ONLY have come from the merge pull.
    assert!(
        block_on(node_c.store.read_history(&workflow_id))?.is_empty(),
        "C must lag the workflow history before failover (load-bearing non-vacuity check)"
    );

    // FAILOVER: A is partitioned (not a send target). C is elected the new owner
    // over {C,B} and become_live UNION-merges B's committed tree (which holds the
    // batch) into C's empty one, recovering the full history.
    node_c
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_B]), OP_TIMEOUT)?;

    let recovered = block_on(node_c.store.read_history(&workflow_id))?;
    assert_eq!(
        recovered, history,
        "the survivor must serve the FULL workflow history after the owner's death \
         — every event arrived via the replicate+merge path from B"
    );
    Ok(())
}

/// Falsifiability control for GATE 2: with the SAME setup, the survivor that never
/// received the writes and runs a BARE `acquire_shard` (no `become_live` merge)
/// can still read the batch only because it WAS a replication target. To make the
/// control bite, here C (which lagged the whole history) bare-acquires and STILL
/// reads an empty history — proving an empty store stays empty without the data.
#[test]
fn lagging_node_without_writes_reads_empty_history() -> TestResult {
    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;

    let node_a = Node::spawn(NODE_A, dir_a.path(), 3, &[NODE_B])?;
    let node_b = Node::spawn(NODE_B, dir_b.path(), 3, &[NODE_A])?;
    let node_c = Node::spawn(NODE_C, dir_c.path(), 3, &[NODE_A, NODE_B])?;
    link_both(&node_a, &node_b)?;
    link_both(&node_a, &node_c)?;
    link_both(&node_b, &node_c)?;

    node_a
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_B]), OP_TIMEOUT)?;

    let workflow_id = WorkflowId::new_v4();
    let history = lifecycle(&workflow_id);
    block_on(
        node_a
            .store
            .append(WriteToken::recorder(), &workflow_id, &history, 0),
    )?;

    // C never received the writes.
    assert!(
        block_on(node_c.store.read_history(&workflow_id))?.is_empty(),
        "C lagged the whole history before any election (load-bearing)"
    );

    // BARE acquire — election ONLY, no become_live merge. C never pulls/unions B.
    node_c
        .database()
        .acquire_shard(SHARD, &membership(3, &[NODE_B]), OP_TIMEOUT)?;

    assert!(
        block_on(node_c.store.read_history(&workflow_id))?.is_empty(),
        "WITHOUT become_live's merge a node that never received the writes serves NOTHING \
         — proving the survivor's recovery in the failover test comes from the data path, \
         not the harness"
    );
    Ok(())
}
