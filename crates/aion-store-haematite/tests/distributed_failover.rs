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

use aion_core::{ContentType, Event, EventEnvelope, Payload, RunId, TimerId, WorkflowId};
use aion_store::{ReadableEventStore, StoreError, WritableEventStore, WriteToken};
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
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => panic!("build runtime: {error}"),
    };
    runtime.block_on(future)
}

const NODE_A: &str = "node-a@127.0.0.1";
const NODE_B: &str = "node-b@127.0.0.1";
const NODE_C: &str = "node-c@127.0.0.1";
const NODE_D: &str = "node-d@127.0.0.1";
const NODE_E: &str = "node-e@127.0.0.1";

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
        executor_threads: None,
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
        send_targets: send_targets
            .iter()
            .map(|name| SyncNodeId::from(*name))
            .collect(),
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
            name.to_owned(),
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

    node_a.database().acquire_shard_and_serve(
        SHARD,
        &membership(3, &[NODE_B, NODE_C]),
        OP_TIMEOUT,
    )?;

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

// ===========================================================================
// SS-3 — SHARD-OWNER DIRECTORY: the adopter's ownership is published to and
// read by a DIFFERENT survivor (gap #2 at the store layer).
// ===========================================================================

/// The store-layer proof for SS-3 gap #2. A is the declared owner of the shard;
/// A is partitioned away (its host "dies"). B adopts the shard — wins the
/// election over the survivors AND `publish_shard_owner`s itself as the new
/// owner. Then C — a DIFFERENT survivor that did NOT adopt — must read the
/// adopter's identity (NODE_B) from its OWN locally-applied replica of the
/// directory record. This is exactly what closes gap #2: without the published
/// record, C resolves the dead declared owner A to "unknown" and a request for
/// the shard's workflows fails locally; with it, C resolves the shard to B (the
/// adopter) and can forward there.
///
/// NON-VACUOUS: C is asserted to have NO directory record before the adoption
/// (`read_shard_owner` is `None`), so the NODE_B it reads afterward could ONLY
/// have arrived via B's quorum-replicated publish.
#[test]
fn adopted_shard_owner_is_published_to_and_read_by_a_different_survivor() -> TestResult {
    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;

    // A is the declared owner and replicates to {B}; B and C are survivors.
    let node_a = Node::spawn(NODE_A, dir_a.path(), 3, &[NODE_B])?;
    let node_b = Node::spawn(NODE_B, dir_b.path(), 3, &[NODE_A, NODE_C])?;
    let node_c = Node::spawn(NODE_C, dir_c.path(), 3, &[NODE_A, NODE_B])?;
    link_both(&node_a, &node_b)?;
    link_both(&node_a, &node_c)?;
    link_both(&node_b, &node_c)?;

    // A is the original fenced owner of the shard.
    node_a
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_B]), OP_TIMEOUT)?;

    // FALSIFIABILITY: no node has published a directory record yet, so a DIFFERENT
    // survivor (C) sees no current owner — ownership is still described only by
    // static config (which names the now-doomed A).
    assert_eq!(
        node_c.store.read_shard_owner(SHARD)?,
        None,
        "before adoption there is no published owner record (load-bearing non-vacuity)"
    );

    // "A dies": A is no longer a send target. B adopts the shard over the
    // surviving quorum {B,C} — election win + become_live merge — then publishes
    // ITSELF as the shard's current owner (the directory write the engine's
    // adopt_shards drives in production).
    node_b
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_C]), OP_TIMEOUT)?;
    node_b.store.publish_shard_owner(SHARD)?;

    // GAP #2 CLOSED: C — which did NOT adopt — reads the adopter's identity off
    // its OWN replica of the quorum-replicated directory record. So a request
    // reaching C now resolves the shard to B (the adopter), not the dead A.
    assert!(
        wait_until(OP_TIMEOUT, || node_c
            .store
            .read_shard_owner(SHARD)
            .ok()
            .flatten()
            .as_deref()
            == Some(NODE_B)),
        "a different survivor (C) must read the ADOPTER (B) as the shard's current owner \
         from the quorum-replicated directory record — this is the gap-#2 fix"
    );

    // The adopter itself also reads its own published record (idempotent self-view).
    assert_eq!(
        node_b.store.read_shard_owner(SHARD)?.as_deref(),
        Some(NODE_B),
        "the adopter reads back its own published ownership"
    );

    // Idempotent re-publish (a second adopt tick) succeeds and is stable.
    node_b.store.publish_shard_owner(SHARD)?;
    assert_eq!(
        node_b.store.read_shard_owner(SHARD)?.as_deref(),
        Some(NODE_B),
        "re-publishing the same owner is a value-preserving no-op CAS"
    );
    Ok(())
}

// ===========================================================================
// #82 — DURABLE TIMERS SURVIVE SHARD ADOPTION: a workflow's durable timer is
// a STAMPED, co-located envelope, so a survivor can adopt the shard (whose
// become_live → merge_committed_union decodes EVERY committed entry as a
// StampedEntry) WITHOUT a HandoffMergeError::UndecodableEntry, and the timer
// is readable post-adoption.
// ===========================================================================

/// Regression guard for failover-correctness bug #82. Before the fix,
/// `schedule_timer` wrote the durable timer through an UNSTAMPED `put_routed` +
/// `commit`. Shard adoption's `become_live` → `merge_committed_union` decodes
/// every committed entry on the shard as a `StampedEntry`, so the unstamped timer
/// failed to decode (`UndecodableEntry`), wedging adoption of ANY shard carrying a
/// durable timer.
///
/// In distributed mode `schedule_timer` now routes through
/// `Database::replicate_write_routed` — a STAMPED, quorum-replicated envelope
/// co-located on the workflow's shard. This test schedules a durable timer on the
/// owner A (replicated to B), partitions A away, and has the survivor C adopt the
/// shard via `acquire_shard_and_serve` (the become_live merge path). The adoption
/// MUST succeed (no UndecodableEntry) and the timer MUST be readable on C after.
///
/// NON-VACUOUS: C lagged the timer write entirely before the failover (its
/// `expired_timers` is empty), so a timer it serves after becoming live could ONLY
/// have arrived through the replicate + union-merge path.
#[test]
fn durable_timer_survives_shard_adoption() -> TestResult {
    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;

    // A replicates to {B} ONLY: quorum {A,B} reached, C never receives the write.
    let node_a = Node::spawn(NODE_A, dir_a.path(), 3, &[NODE_B])?;
    let node_b = Node::spawn(NODE_B, dir_b.path(), 3, &[NODE_A])?;
    let node_c = Node::spawn(NODE_C, dir_c.path(), 3, &[NODE_A, NODE_B])?;
    link_both(&node_a, &node_b)?;
    link_both(&node_a, &node_c)?;
    link_both(&node_b, &node_c)?;

    // A is the owner of the shard and serves it (so its stamps draw a live epoch).
    node_a
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_B]), OP_TIMEOUT)?;

    // A schedules a durable timer for a workflow on the shard. This goes through
    // the DISTRIBUTED path → `replicate_write_routed` (a stamped, co-located
    // envelope), quorum-replicated to B.
    let workflow_id = WorkflowId::new_v4();
    let timer_id = TimerId::anonymous(1);
    let fire_at = chrono::Utc::now();
    block_on(
        node_a
            .store
            .schedule_timer(&workflow_id, &timer_id, fire_at),
    )?;

    // The owner reads its own scheduled timer back (decoded through the read path,
    // which strips the stamp).
    let as_of = fire_at + chrono::Duration::seconds(1);
    let a_timers = block_on(node_a.store.expired_timers(as_of))?;
    assert_eq!(
        a_timers.len(),
        1,
        "the owner must hold its own scheduled durable timer"
    );

    // FALSIFIABILITY: C lagged the timer write before the failover, so anything it
    // serves afterward could ONLY have arrived via the merge pull from B.
    assert!(
        block_on(node_c.store.expired_timers(as_of))?.is_empty(),
        "C must lag the durable timer before adoption (load-bearing non-vacuity check)"
    );

    // FAILOVER + ADOPTION: A is partitioned (not a send target). C adopts the shard
    // over {C,B}. `acquire_shard_and_serve` runs become_live → merge_committed_union,
    // which decodes EVERY committed entry on the shard — including the timer — as a
    // StampedEntry. Pre-fix this returned HandoffMergeError::UndecodableEntry; with
    // the stamped timer it MUST succeed.
    node_c
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_B]), OP_TIMEOUT)?;

    // The adopter now owns the shard, so its scoped scan covers it. The timer is
    // readable post-adoption — it travelled with the shard through the union merge.
    let recovered = block_on(node_c.store.expired_timers(as_of))?;
    assert_eq!(
        recovered.len(),
        1,
        "the survivor must serve the durable timer after adopting the shard — the \
         stamped, co-located timer survived become_live's union merge (bug #82 fixed)"
    );
    assert_eq!(
        recovered[0].workflow_id, workflow_id,
        "the recovered timer is the one A scheduled"
    );
    assert_eq!(
        recovered[0].timer_id, timer_id,
        "the recovered timer's id round-trips through replicate + merge unchanged"
    );
    Ok(())
}

// ===========================================================================
// ADR-021 — DOUBLE-ADOPTION FENCE: a survivor deposed by a HIGHER-ballot owner
// gets the typed fence on `publish_shard_owner` (→ StoreError::NotOwner), the
// abort signal the engine's clean-partial adopt order keys off to drop the shard
// WITHOUT widening scope or recovering it.
// ===========================================================================

/// Store-layer proof of the publish-fence abort. In a FIVE-node cluster, B adopts
/// the shard (acquire + publish), then C wins a STRICTLY HIGHER-ballot election
/// over a quorum that EXCLUDES B ({C,D,E}) — deposing B without raising B's own
/// local promised epoch. B's NEXT fenced directory write is therefore
/// quorum-REJECTED (not locally self-fenced) and surfaces as the typed
/// `StoreError::NotOwner`, exactly the signal `adopt_shards_inner` keys off to
/// drop a deposed survivor's shard.
///
/// A five-node topology is load-bearing: in a 3-node cluster with one dead node,
/// any deposing election MUST use the deposed survivor itself as a promiser
/// (raising its local epoch), so its later publish fails at LOCAL commit rather
/// than via the typed quorum fence. Five nodes let C form a majority {C,D,E}
/// WITHOUT B, isolating the typed `Fenced` quorum-reject path the design targets.
///
/// NON-VACUOUS: B's FIRST publish (before C's election) succeeds, proving B was a
/// real owner whose authority was revoked by C's higher ballot — not a publish
/// that never had a chance.
#[test]
fn deposed_survivor_publish_is_fenced_to_not_owner() -> TestResult {
    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;
    let dir_d = tempfile::tempdir()?;
    let dir_e = tempfile::tempdir()?;

    // Five nodes, quorum = 3. A is the doomed declared owner.
    let node_a = Node::spawn(NODE_A, dir_a.path(), 5, &[NODE_B, NODE_C, NODE_D])?;
    // B replicates its publish to {C,D,E}: a quorum that, once it has promised C's
    // higher ballot, deterministically REJECTS B's fenced write.
    let node_b = Node::spawn(NODE_B, dir_b.path(), 5, &[NODE_C, NODE_D, NODE_E])?;
    // C deposes B over {D,E} (a majority {C,D,E}) — B is NOT a promiser, so B's
    // own local promised epoch is never raised by C's election.
    let node_c = Node::spawn(NODE_C, dir_c.path(), 5, &[NODE_D, NODE_E])?;
    let node_d = Node::spawn(NODE_D, dir_d.path(), 5, &[NODE_C, NODE_E])?;
    let node_e = Node::spawn(NODE_E, dir_e.path(), 5, &[NODE_C, NODE_D])?;

    // Full mesh so every Prepare/WriteProposal reaches its targets.
    let nodes = [&node_a, &node_b, &node_c, &node_d, &node_e];
    for (i, from) in nodes.iter().enumerate() {
        for to in nodes.iter().skip(i + 1) {
            link_both(from, to)?;
        }
    }

    // A is the original fenced owner; then "dies".
    node_a.database().acquire_shard_and_serve(
        SHARD,
        &membership(5, &[NODE_B, NODE_C, NODE_D]),
        OP_TIMEOUT,
    )?;

    // B adopts: wins the election over {C,D} and publishes itself. The first
    // publish SUCCEEDS — B is a real owner (load-bearing non-vacuity).
    node_b.database().acquire_shard_and_serve(
        SHARD,
        &membership(5, &[NODE_C, NODE_D]),
        OP_TIMEOUT,
    )?;
    node_b.store.publish_shard_owner(SHARD)?;

    // C now wins a STRICTLY HIGHER-ballot election over {D,E} — a majority {C,D,E}
    // that EXCLUDES B, so B's local promised epoch is untouched.
    node_c.database().acquire_shard_and_serve(
        SHARD,
        &membership(5, &[NODE_D, NODE_E]),
        OP_TIMEOUT,
    )?;

    // B's NEXT fenced publish proposes to {C,D,E} — all of which promised C's
    // higher ballot — so it is quorum-REJECTED (typed Fenced) and the store maps
    // it to the retryable NotOwner: the ABORT signal the adopt order drops on.
    let outcome = node_b.store.publish_shard_owner(SHARD);
    assert_eq!(
        outcome,
        Err(StoreError::NotOwner { shard: SHARD }),
        "a deposed survivor's fenced publish must surface as NotOwner (the adopt-abort signal)"
    );
    Ok(())
}
