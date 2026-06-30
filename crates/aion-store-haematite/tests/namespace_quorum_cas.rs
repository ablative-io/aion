//! #160 — the DISTRIBUTED quorum-CAS branches of the namespace registry, driven
//! through a REAL multi-node haematite cluster (real membership, real quorum,
//! real `replicate_write`). No mocks, no hand-rolled CAS.
//!
//! `HaematiteStore::register_namespace_record`'s distributed path mirrors the
//! #157-hardened `publish_shard_owner` exactly:
//!
//! * `replicate_write` (create-if-absent) commits ⇒ `MintOutcome::Created`.
//! * `DatabaseError::CasConflict` (a concurrent racer minted/touched first) ⇒
//!   idempotent `MintOutcome::AlreadyExisted`.
//! * `DatabaseError::Fenced` (a higher ballot deposed this node) ⇒
//!   `StoreError::NotOwner` — a retryable signal, never a silent success or a
//!   duplicate.
//!
//! These two branches rested on parity with `publish_shard_owner`; here they are
//! exercised DIRECTLY against the namespace key (`n:`), end to end:
//!
//! GATE 1 — CONVERGENCE (`CasConflict` branch): the SAME namespace name is
//! registered across nodes and converges to EXACTLY ONE durable record — exactly
//! one `Created`, every other register `AlreadyExisted`, and `get_namespace` on
//! ANY node returns the single replicated record. The create-path `CasConflict`
//! is driven through a real lagging follower whose create-if-absent is
//! quorum-REJECTED because the quorum already holds the value.
//!
//! GATE 2 — FENCE (Fenced branch): a register issued against a node that is NOT
//! the live owner of the namespace's shard — deposed by a strictly higher ballot
//! — surfaces `StoreError::NotOwner`, the retryable re-route signal. Driven
//! through the SAME real ownership/fence machinery the failover tests use, never
//! a fabricated `Fenced`.

//! # Why plain `#[test]` (not `#[tokio::test]`)
//!
//! haematite's distribution coordinator (`bind`, `connect`, `acquire_shard_*`,
//! `replicate_write`) BLOCKS and refuses to run from a thread with an entered
//! tokio runtime. So the cluster lifecycle runs on the bare test thread, the
//! synchronous inherent `register_namespace_record` runs there too (it spawns its
//! own bare thread for the quorum wait), and the ASYNC `get_namespace` reads are
//! driven through a transient runtime's `block_on` that exits its context the
//! moment it returns.

#![allow(clippy::expect_used)]

use std::error::Error;
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aion_store::{MintOutcome, NamespaceOrigin, NamespaceRecord, NamespaceStore, StoreError};
use aion_store_haematite::HaematiteStore;
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig};

type TestResult = Result<(), Box<dyn Error>>;

const NODE_A: &str = "node-a@127.0.0.1";
const NODE_B: &str = "node-b@127.0.0.1";
const NODE_C: &str = "node-c@127.0.0.1";
const NODE_D: &str = "node-d@127.0.0.1";
const NODE_E: &str = "node-e@127.0.0.1";

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OP_TIMEOUT: Duration = Duration::from_secs(5);

/// The namespace's shard. The store runs `shard_count == 1`, so every namespace
/// key (`n: || name`) routes to shard 0 — the shard whose ownership fence GATE 2
/// drives a register against.
const SHARD: usize = 0;

const NAMESPACE: &str = "orders";

/// Drive one async adapter future to completion on a fresh runtime, then drop the
/// runtime so the next (blocking) haematite call runs outside any runtime context.
fn block_on<F: Future>(future: F) -> Result<F::Output, Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    Ok(runtime.block_on(future))
}

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
        send_targets: send_targets
            .iter()
            .map(|name| SyncNodeId::from(*name))
            .collect(),
    }
}

/// One cluster node: a distributed `HaematiteStore` over a distribution-attached
/// `Database`, plus a background responder draining and answering inbound
/// replication / election traffic on the SAME `Database` the store writes to.
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

/// A freshly-minted worker-origin record for `name`.
fn minted(name: &str) -> NamespaceRecord {
    NamespaceRecord::new_minted(name, NamespaceOrigin::WorkerMint, chrono::Utc::now())
}

/// Register `name` against `node`'s distributed store through the SAME quorum-CAS
/// inherent path the `NamespaceStore` trait wrapper drives.
fn register(node: &Node, name: &str) -> Result<MintOutcome, StoreError> {
    node.store.register_namespace_record(&minted(name))
}

/// `get_namespace` on `node`, driven through a transient runtime.
fn get(node: &Node, name: &str) -> Result<Option<NamespaceRecord>, Box<dyn Error>> {
    block_on(node.store.get_namespace(name))?.map_err(Into::into)
}

// ===========================================================================
// GATE 1 — CONVERGENCE: the SAME namespace registered across nodes converges to
// EXACTLY ONE durable record (the create-path CasConflict branch).
// ===========================================================================

/// A is the owner and replicates `orders` to a quorum {B}; C LAGS the write
/// entirely. C then registers the SAME name: its local read sees absent, so it
/// takes the create-if-absent path — but the quorum already holds the value, so
/// the proposal is quorum-REJECTED as a value-CAS mismatch (`CasConflict`), which
/// the store maps to the idempotent `MintOutcome::AlreadyExisted`. The cluster
/// converges to A's single record — readable on the quorum that holds it
/// (including B, a replication target that did NOT originate it) — and C's
/// rejected create forks NO second record (it stays absent on C).
///
/// NON-VACUOUS: C is asserted to read NO record before its register (it lagged),
/// so its `AlreadyExisted` could ONLY come from the quorum rejecting its create
/// against the value the quorum already holds — not from C observing it locally.
#[test]
fn duplicate_register_across_nodes_converges_to_one_record() -> TestResult {
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

    // NO shard election here: with no live owner every node stamps the bottom
    // epoch, so the per-write fence is a no-op (`bottom >= bottom` accepts) and a
    // duplicate create is decided purely on the VALUE-CAS — the create-path
    // `CasConflict` branch (GATE 2 covers the fenced `NotOwner` branch instead).

    // FIRST register: A mints the namespace, quorum-replicated to {B}.
    assert_eq!(
        register(&node_a, NAMESPACE)?,
        MintOutcome::Created,
        "the first register mints the namespace (create-if-absent commits)"
    );
    let canonical = get(&node_a, NAMESPACE)?.expect("owner must read its own minted record");

    // FALSIFIABILITY: C lagged the write entirely, so it sees no record locally.
    // Whatever its register reports can only come from the quorum-CAS path.
    assert_eq!(
        get(&node_c, NAMESPACE)?,
        None,
        "C must lag the namespace record before its register (load-bearing non-vacuity)"
    );

    // C registers the SAME name: local-absent ⇒ create-if-absent ⇒ quorum holds
    // the value ⇒ CasConflict ⇒ idempotent AlreadyExisted (the create-path branch).
    assert_eq!(
        register(&node_c, NAMESPACE)?,
        MintOutcome::AlreadyExisted,
        "a lagging follower's duplicate create is quorum-rejected to AlreadyExisted (CasConflict)"
    );

    // A re-register on the owner (local-present ⇒ value-CAS touch) is also
    // AlreadyExisted: every duplicate, by either path, is idempotent.
    assert_eq!(
        register(&node_a, NAMESPACE)?,
        MintOutcome::AlreadyExisted,
        "a duplicate register on the owner is the idempotent touch branch"
    );

    // CONVERGENCE: exactly ONE durable record. It is readable on the quorum that
    // holds it — including B, a replication target that did NOT originate the
    // write, proving the record genuinely replicated (idempotent on "any node").
    assert_record_is(&node_a, NAMESPACE, &canonical)?;
    assert_record_is(&node_b, NAMESPACE, &canonical)?;

    // C's quorum-rejected duplicate create minted NOTHING locally: it observed the
    // existing record via the CAS reject, never forking a second/divergent record.
    // So the cluster converged to exactly one record, not two.
    assert_eq!(
        get(&node_c, NAMESPACE)?,
        None,
        "C's rejected duplicate create must NOT have minted a divergent local record"
    );
    Ok(())
}

/// Assert `node` reads exactly the ONE canonical `expected` record for `name`
/// (identity by name, origin, and creation instant), waiting out replication.
fn assert_record_is(node: &Node, name: &str, expected: &NamespaceRecord) -> TestResult {
    if !wait_until(OP_TIMEOUT, || matches!(get(node, name), Ok(Some(_)))) {
        return Err(format!("{} never converged on a record for {name}", node.name).into());
    }
    let observed =
        get(node, name)?.ok_or_else(|| format!("{} lost the record for {name}", node.name))?;
    assert_eq!(
        observed.name, expected.name,
        "{} must read the single replicated namespace record",
        node.name
    );
    assert_eq!(
        observed.origin, expected.origin,
        "{} must observe the canonical record's origin (no divergent mint)",
        node.name
    );
    assert_eq!(
        observed.created_at, expected.created_at,
        "{} must observe the canonical record's creation instant (one durable record)",
        node.name
    );
    Ok(())
}

// ===========================================================================
// GATE 2 — FENCE: a register against a node that is NOT the namespace shard's
// live owner — deposed by a higher ballot — surfaces NotOwner (Fenced branch).
// ===========================================================================

/// Store-layer proof of the register fence. In a FIVE-node cluster, B is the live
/// owner of the namespace's shard (acquire + a successful register, proving its
/// authority). C then wins a STRICTLY HIGHER-ballot election over a quorum that
/// EXCLUDES B ({C,D,E}) — deposing B WITHOUT raising B's own local promised epoch.
/// B's NEXT register is therefore quorum-REJECTED by the fence (typed `Fenced`,
/// not a local self-fence) and the store maps it to the retryable
/// `StoreError::NotOwner` — never a silent success or a duplicate record.
///
/// A five-node topology is load-bearing: in a 3-node cluster with one dead node,
/// any deposing election MUST use the deposed owner itself as a promiser (raising
/// its local epoch), so its later register would fail at LOCAL commit rather than
/// via the typed quorum fence. Five nodes let C form a majority {C,D,E} WITHOUT B,
/// isolating the typed `Fenced` quorum-reject path the design targets.
///
/// NON-VACUOUS: B's FIRST register (before C's election) SUCCEEDS, proving B was a
/// real owner whose authority was revoked by C's higher ballot — not a register
/// that never had a chance.
#[test]
fn register_against_deposed_owner_is_fenced_to_not_owner() -> TestResult {
    let dirs: Vec<_> = (0..5)
        .map(|_| tempfile::tempdir())
        .collect::<Result<_, _>>()?;
    let nodes = spawn_five(&dirs)?;
    let [node_a, node_b, node_c, node_d, node_e] = &nodes;

    // Full mesh so every Prepare / WriteProposal reaches its targets.
    let refs = [node_a, node_b, node_c, node_d, node_e];
    for (index, from) in refs.iter().enumerate() {
        for to in refs.iter().skip(index + 1) {
            link_both(from, to)?;
        }
    }

    // A is the original fenced owner of the namespace's shard; then "dies".
    node_a.database().acquire_shard_and_serve(
        SHARD,
        &membership(5, &[NODE_B, NODE_C, NODE_D]),
        OP_TIMEOUT,
    )?;

    // B adopts the shard (wins over {C,D}) and registers the namespace. The first
    // register SUCCEEDS — B is a real, live owner (load-bearing non-vacuity).
    node_b.database().acquire_shard_and_serve(
        SHARD,
        &membership(5, &[NODE_C, NODE_D]),
        OP_TIMEOUT,
    )?;
    assert!(
        matches!(register(node_b, NAMESPACE)?, MintOutcome::Created),
        "B's first register must succeed — it is the live owner (non-vacuity)"
    );

    // C wins a STRICTLY HIGHER-ballot election over {D,E} — a majority {C,D,E}
    // that EXCLUDES B, so B's local promised epoch is untouched.
    node_c.database().acquire_shard_and_serve(
        SHARD,
        &membership(5, &[NODE_D, NODE_E]),
        OP_TIMEOUT,
    )?;

    // B's NEXT register proposes to {C,D,E} — all of which promised C's higher
    // ballot — so it is quorum-REJECTED (typed Fenced) and the store maps it to
    // the retryable NotOwner: never a silent success, never a duplicate record.
    assert_eq!(
        register(node_b, NAMESPACE),
        Err(StoreError::NotOwner { shard: SHARD }),
        "a deposed owner's fenced register must surface as NotOwner (the retryable re-route signal)"
    );
    Ok(())
}

/// Spawn the five mesh nodes {A..E}, quorum = 3, each replicating to a quorum that
/// makes the GATE 2 fence deterministic (see the test's topology rationale).
fn spawn_five(dirs: &[tempfile::TempDir]) -> Result<[Node; 5], Box<dyn Error>> {
    let node_a = Node::spawn(NODE_A, dirs[0].path(), 5, &[NODE_B, NODE_C, NODE_D])?;
    let node_b = Node::spawn(NODE_B, dirs[1].path(), 5, &[NODE_C, NODE_D, NODE_E])?;
    let node_c = Node::spawn(NODE_C, dirs[2].path(), 5, &[NODE_D, NODE_E])?;
    let node_d = Node::spawn(NODE_D, dirs[3].path(), 5, &[NODE_C, NODE_E])?;
    let node_e = Node::spawn(NODE_E, dirs[4].path(), 5, &[NODE_C, NODE_D])?;
    Ok([node_a, node_b, node_c, node_d, node_e])
}
