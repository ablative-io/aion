//! SS-3 end-to-end: the request-routing `ShardDirectory` resolves a survivor
//! that ADOPTED a dead declared owner's shard to the adopter — closing gap #2.
//!
//! Gap #2 (kill-9 failover demo, Phase B step 5): after a survivor's SS-5b
//! supervisor adopts a dead owner's shard, a client request reaching a DIFFERENT
//! survivor cannot be routed to the adopter. The static directory resolved the
//! down declared owner to `OwnerView::Unknown` → routed locally →
//! `WorkflowNotFound`, because static config has no "who adopted this shard now"
//! signal.
//!
//! SS-3 closes that: the adopter `publish_shard_owner`s itself (quorum-replicated,
//! fenced) and every other survivor's `StaticShardDirectory::owner_of` reads that
//! record off its own replica and returns the adopter as `OwnerView::Remote`, so
//! `route_mutation` forwards there instead of failing locally.
//!
//! This test stands up a real 3-node haematite cluster (REAL beamr loopback
//! transport), builds the production `StaticShardDirectory` over a survivor's
//! store + static peer config (the SAME wiring `build_routing_state` does), kills
//! the declared owner, has a survivor adopt + publish, and asserts the OTHER
//! survivor's directory now resolves the shard to the adopter — then confirms
//! `route_mutation` forwards there rather than returning the gap-#2 `NotOwner`.
//!
//! Behind `--features haematite-backend` (the directory lives there). Plain
//! `#[test]`: haematite's distribution coordinator blocks and refuses to run from
//! a thread with an entered tokio runtime, exactly as `distributed_failover.rs`.

#![cfg(feature = "haematite-backend")]
#![allow(clippy::panic)]

use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aion_core::WorkflowId;
use aion_server::routing::{
    DirectoryPeer, OwnerView, RouteDecision, ShardDirectory, StaticShardDirectory, route_mutation,
};
use aion_store_haematite::HaematiteStore;
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};

type TestResult = Result<(), Box<dyn Error>>;

const NODE_A: &str = "node-a@127.0.0.1";
const NODE_B: &str = "node-b@127.0.0.1";
const NODE_C: &str = "node-c@127.0.0.1";

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);

/// The shard A declares ownership of; B adopts it after A "dies".
const SHARD: usize = 1;
/// A fixed gRPC forward address for the adopter peer (B). Never dialed in this
/// test — the assertion is that the directory RESOLVES B with this address.
const NODE_B_GRPC: &str = "127.0.0.1:50052";

fn loopback() -> Result<SocketAddr, Box<dyn Error>> {
    Ok("127.0.0.1:0".parse()?)
}

fn config_for(path: &std::path::Path) -> DatabaseConfig {
    DatabaseConfig {
        data_dir: path.to_path_buf(),
        // Enough shards that owned/declared shards are a real subset, so a
        // non-owned shard genuinely consults the directory rather than own-all.
        shard_count: 3,
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
        send_targets: send_targets
            .iter()
            .map(|name| SyncNodeId::from(*name))
            .collect(),
    }
}

/// One cluster node: a distributed `HaematiteStore` over a distribution-attached
/// `Database`, plus a background responder answering peers' replication/election.
struct Node {
    store: Arc<HaematiteStore>,
    event_store: Arc<haematite::EventStore>,
    addr: SocketAddr,
    name: &'static str,
    responder: Option<JoinHandle<()>>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl Node {
    fn spawn(
        name: &'static str,
        dir: &std::path::Path,
        total_nodes: usize,
        send_targets: &[&str],
    ) -> Result<Self, Box<dyn Error>> {
        let endpoint = DistributionEndpoint::bind(name, loopback()?, 1, None)?;
        let addr = endpoint.local_addr();
        let database =
            Database::create(config_for(dir.join("db").as_path()))?.with_distribution(endpoint);
        let store = Arc::new(HaematiteStore::with_distribution(
            database,
            membership(total_nodes, send_targets),
            OP_TIMEOUT,
            name.to_owned(),
        ));
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

/// Build the production directory for node C (a survivor that did NOT adopt the
/// shard), with the SAME static peer config a real boot's `build_routing_state`
/// assembles: A and B are peers, A statically declares SHARD, B carries a gRPC
/// forward address. This is the directory a request reaching C consults.
fn directory_on_c(node_c: &Node, node_b_grpc: SocketAddr) -> StaticShardDirectory {
    StaticShardDirectory::new(
        Arc::clone(&node_c.store),
        vec![
            DirectoryPeer {
                name: NODE_A.to_owned(),
                owned_shards: vec![SHARD],
                grpc_addr: None,
            },
            DirectoryPeer {
                name: NODE_B.to_owned(),
                // B's own shard is 2 here; it ADOPTS SHARD at runtime, which the
                // static config does not know — exactly the gap-#2 setup.
                owned_shards: vec![2],
                grpc_addr: Some(node_b_grpc),
            },
        ],
        Some(NODE_C.to_owned()),
    )
}

/// The headline SS-3 gap-#2 proof. C is a survivor that did NOT adopt SHARD.
/// Before adoption, C's directory resolves SHARD's dead declared owner (A) to
/// `Unknown` → `route_mutation` routes LOCALLY (the gap-#2 failure: C does not
/// own SHARD, so the engine would return `WorkflowNotFound`). After B adopts SHARD
/// and publishes itself, C's directory resolves SHARD to B (`Remote` with B's
/// gRPC address) → `route_mutation` FORWARDS to B (the adopter).
#[test]
fn directory_resolves_adopter_after_failover_so_request_forwards() -> TestResult {
    let dir_a = tempfile::tempdir()?;
    let dir_b = tempfile::tempdir()?;
    let dir_c = tempfile::tempdir()?;

    // A declares + owns SHARD and replicates to {B}; B and C are survivors.
    let node_a = Node::spawn(NODE_A, dir_a.path(), 3, &[NODE_B])?;
    let node_b = Node::spawn(NODE_B, dir_b.path(), 3, &[NODE_A, NODE_C])?;
    let node_c = Node::spawn(NODE_C, dir_c.path(), 3, &[NODE_A, NODE_B])?;
    link_both(&node_a, &node_b)?;
    link_both(&node_a, &node_c)?;
    link_both(&node_b, &node_c)?;

    // C owns its own shard (0), NOT SHARD — so a request for SHARD's workflows is
    // genuinely non-local on C and must consult the directory.
    node_c.store.set_owned_shards([0]);
    // B owns its own shard (2) at first.
    node_b.store.set_owned_shards([2]);

    // A is the original fenced owner of SHARD.
    node_a
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_B]), OP_TIMEOUT)?;

    let node_b_grpc: SocketAddr = NODE_B_GRPC.parse()?;
    let directory = directory_on_c(&node_c, node_b_grpc);

    // A workflow id whose durable shard is SHARD, so routing for it consults the
    // SHARD directory entry. (Probe v4 ids — the same way the edge tests do.)
    let mut wf_on_shard = None;
    for _ in 0..100_000 {
        let candidate = WorkflowId::new_v4();
        if node_c.store.shard_for_workflow(&candidate) == SHARD {
            wf_on_shard = Some(candidate);
            break;
        }
    }
    let workflow_id = wf_on_shard.ok_or("no workflow id landed on SHARD")?;

    // BEFORE adoption: the declared owner A is still linked-live to C here, so the
    // static map resolves A as the (live) remote owner. The load-bearing gap-#2
    // condition is what happens once A is GONE and B has adopted — asserted below.
    // First, prove the directory record is absent (non-vacuity).
    assert_eq!(
        node_c.store.read_shard_owner(SHARD)?,
        None,
        "no published owner record before adoption (non-vacuity)"
    );

    // "A dies": drop A so its links to B and C tear down (peer_connected flips
    // false), exactly as a kill-9 would. B then adopts SHARD over the surviving
    // quorum {B,C} and publishes itself as the new owner.
    drop(node_a);
    assert!(
        wait_until(OP_TIMEOUT, || !node_c.store.peer_connected(NODE_A)),
        "C must observe A's link drop (the failover trigger)"
    );

    node_b
        .database()
        .acquire_shard_and_serve(SHARD, &membership(3, &[NODE_C]), OP_TIMEOUT)?;
    node_b.store.extend_owned_shards([SHARD]);
    node_b.store.publish_shard_owner(SHARD)?;

    // GAP #2 CLOSED: C reads the adopter (B) from the replicated directory record.
    assert!(
        wait_until(OP_TIMEOUT, || node_c
            .store
            .read_shard_owner(SHARD)
            .ok()
            .flatten()
            .as_deref()
            == Some(NODE_B)),
        "C must read the adopter (B) from the quorum-replicated directory record"
    );

    // The directory resolves SHARD to B as a forwardable Remote (B is live to C),
    // NOT to the dead declared owner A and NOT to Unknown.
    let view = directory.owner_of(SHARD);
    match &view {
        OwnerView::Remote(node) => {
            assert_eq!(node.node_id, NODE_B, "owner resolves to the ADOPTER B");
            assert_eq!(
                node.grpc_addr,
                Some(node_b_grpc),
                "the resolved owner carries B's gRPC forward address"
            );
        }
        other => panic!("expected Remote(adopter B), got {other:?}"),
    }

    // And `route_mutation` therefore FORWARDS a request for that workflow to B —
    // the availability behaviour gap #2 was blocking (previously it routed Local
    // and the engine returned WorkflowNotFound on the non-owning survivor).
    let decision = route_mutation(Some(node_c.store.as_ref()), Some(&directory), &workflow_id);
    match decision {
        RouteDecision::Forward { owner, shard } => {
            assert_eq!(owner.node_id, NODE_B, "forward target is the adopter B");
            assert_eq!(shard, SHARD);
        }
        other => panic!("expected Forward to the adopter, got {other:?}"),
    }
    Ok(())
}
