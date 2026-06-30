//! SS-5b: automatic multi-node failover detection.
//!
//! [`ClusterSupervisor`] is the production counterpart to the manual
//! `Engine::adopt_shards` trigger proven in the SS-5 demo. It runs a background
//! task that watches the liveness of every peer that owns shards and, when a
//! peer's replication link drops and stays down past a debounce threshold,
//! calls `adopt_shards` for that peer's shards ITSELF — no human in the loop.
//!
//! ## How peer-down is detected
//!
//! The liveness signal is the haematite distribution link state
//! ([`HaematiteStore::peer_connected`]): beamr's OTP distribution tears the
//! connection down (read-loop EOF → deregister) the instant the peer's process
//! dies, so `peer_connected` flips to `false` on a real `kill -9` exactly as it
//! does on a graceful drop. It is a true socket-liveness signal, not a heartbeat
//! heuristic.
//!
//! ## Debounce
//!
//! A single missed poll is not a death: a transient blip must not trigger a
//! disruptive shard adoption. The supervisor requires `confirmations`
//! CONSECUTIVE polls observing the peer disconnected before it acts. Any single
//! reconnect observation resets the counter. Once a peer's shards are adopted it
//! is marked handled and not re-adopted while it stays down (adoption is itself
//! idempotent, but re-running it every tick would be wasteful); a later reconnect
//! clears the handled mark so a flapping peer that genuinely dies again is
//! re-adopted.
//!
//! ## Scope
//!
//! Behind the `haematite-backend` feature and only ever constructed for a
//! distributed (`[store.cluster]`) boot. A single-node / non-clustered server
//! never spawns it, so default behaviour is unchanged.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aion::Engine;
use aion_core::ClusterEvent;

use crate::cluster_publisher::ClusterEventPublisher;

/// The liveness signal the supervisor polls. Implemented by [`HaematiteStore`]
/// in production and by a fake in tests, so the debounce/adopt logic is verified
/// without standing up a real cluster every time.
pub trait PeerLiveness: Send + Sync + 'static {
    /// Whether the peer named `peer_name` currently holds a live replication link.
    fn peer_connected(&self, peer_name: &str) -> bool;

    /// The distribution name currently RECORDED as `shard`'s owner in the cluster
    /// shard-owner directory (SS-3), or `None` when no record exists. Used by the
    /// adopt pre-check to detect a shard already adopted-and-published by another
    /// survivor, so this supervisor does not race a second adoption of it. Mirrors
    /// `routing::directory::resolve_from_record`'s down-owner detection: a record
    /// naming a LIVE peer means handled-elsewhere (skip); a record naming a peer
    /// that is itself down is adoptable (the recorded owner has since died).
    ///
    /// A failed read returns `None` ("no directory opinion") so a transient read
    /// failure never strands a dead peer's shards.
    fn read_shard_owner(&self, shard: usize) -> Option<String>;
}

#[cfg(feature = "haematite-backend")]
impl PeerLiveness for aion_store_haematite::HaematiteStore {
    fn peer_connected(&self, peer_name: &str) -> bool {
        Self::peer_connected(self, peer_name)
    }

    fn read_shard_owner(&self, shard: usize) -> Option<String> {
        // A failed read is "no directory opinion": fall through to adoption.
        Self::read_shard_owner(self, shard).ok().flatten()
    }
}

/// The failover action the supervisor invokes when a peer is confirmed down.
/// Implemented by [`Engine`] in production and by a fake in tests.
#[async_trait::async_trait]
pub trait ShardAdopter: Send + Sync + 'static {
    /// Adopt `shards` from a dead peer: elect + union-merge + resume.
    async fn adopt_shards(&self, shards: &[usize]) -> Result<(), String>;
}

#[async_trait::async_trait]
impl ShardAdopter for Engine {
    async fn adopt_shards(&self, shards: &[usize]) -> Result<(), String> {
        Engine::adopt_shards(self, shards)
            .await
            .map_err(|error| error.to_string())
    }
}

/// One peer the supervisor watches: its distribution name and the shards it owns
/// (which this node will adopt if the peer dies).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchedPeer {
    /// The peer's globally-unique distribution name.
    pub name: String,
    /// The shards this peer owns; adopted on confirmed death.
    pub owned_shards: Vec<usize>,
}

/// Tuning for the supervisor's poll loop.
#[derive(Clone, Copy, Debug)]
pub struct SupervisorConfig {
    /// Interval between liveness polls.
    pub poll_interval: Duration,
    /// Consecutive disconnected observations required before adopting (debounce).
    /// Must be at least one.
    pub confirmations: u32,
}

/// Per-peer debounce state tracked across poll ticks.
#[derive(Default)]
struct PeerState {
    /// Consecutive ticks this peer has been observed disconnected.
    consecutive_down: u32,
    /// Whether this peer's shards have already been adopted while down.
    adopted: bool,
}

/// Watches peer liveness and auto-adopts a dead peer's shards (SS-5b).
pub struct ClusterSupervisor<L: PeerLiveness, A: ShardAdopter> {
    liveness: Arc<L>,
    adopter: Arc<A>,
    peers: Vec<WatchedPeer>,
    config: SupervisorConfig,
    state: BTreeMap<String, PeerState>,
    /// WS3 cluster-event sink. `None` keeps every existing test compiling and
    /// keeps a non-ops-console boot silent; when present, `tick()` emits a delta at
    /// each of its existing branch points. The publisher fans out to live
    /// ops console subscribers and is a no-op with none attached.
    publisher: Option<Arc<ClusterEventPublisher>>,
    /// This node's distribution name, stamped into `ShardAdopted.adopted_by` and
    /// the supervisor lifecycle events. Empty when unknown (no emit honesty cost:
    /// the field is still the real configured value or absent).
    self_node: String,
}

impl<L: PeerLiveness, A: ShardAdopter> ClusterSupervisor<L, A> {
    /// Build a supervisor over `peers`, polling `liveness` and calling
    /// `adopter.adopt_shards` on confirmed peer death. Peers with no owned shards
    /// are dropped from the watch set (nothing to adopt for them).
    #[must_use]
    pub fn new(
        liveness: Arc<L>,
        adopter: Arc<A>,
        peers: Vec<WatchedPeer>,
        config: SupervisorConfig,
    ) -> Self {
        let peers: Vec<WatchedPeer> = peers
            .into_iter()
            .filter(|peer| !peer.owned_shards.is_empty())
            .collect();
        let state = peers
            .iter()
            .map(|peer| (peer.name.clone(), PeerState::default()))
            .collect();
        Self {
            liveness,
            adopter,
            peers,
            config,
            state,
            publisher: None,
            self_node: String::new(),
        }
    }

    /// Attach the WS3 cluster-event publisher and this node's name so `tick()`
    /// emits topology deltas. Pure builder addition — a supervisor without it
    /// behaves exactly as before (every existing test passes `new` only).
    #[must_use]
    pub fn with_publisher(
        mut self,
        publisher: Arc<ClusterEventPublisher>,
        self_node: impl Into<String>,
    ) -> Self {
        self.publisher = Some(publisher);
        self.self_node = self_node.into();
        self
    }

    /// Emit a cluster event through the attached publisher, if any. The `build`
    /// closure receives the publisher-stamped meta; with no publisher attached
    /// this is a no-op.
    fn emit<F>(&self, build: F)
    where
        F: FnOnce(aion_core::ClusterEventMeta) -> ClusterEvent,
    {
        if let Some(publisher) = &self.publisher {
            drop(publisher.emit(build));
        }
    }

    /// Whether this supervisor watches any peer (false when no peer declared
    /// owned shards — the loop would do nothing, so the caller can skip spawning).
    #[must_use]
    pub fn watches_any(&self) -> bool {
        !self.peers.is_empty()
    }

    /// Borrow the adopter (the engine, in production) this supervisor drives.
    /// Lets a test inspect the engine it auto-adopts onto after the loss.
    #[must_use]
    pub fn adopter(&self) -> &A {
        &self.adopter
    }

    /// Run ONE poll tick: observe every watched peer's liveness, advance the
    /// debounce counters, and adopt the shards of any peer that has now been down
    /// for `confirmations` consecutive ticks and is not yet adopted.
    ///
    /// Returned is the list of peer names adopted on THIS tick (empty on a quiet
    /// tick), so a test can assert exactly when adoption fires. Extracted from the
    /// loop so the debounce decision is unit-testable without real time.
    pub async fn tick(&mut self) -> Vec<String> {
        let mut adopted_now = Vec::new();
        // Collect emits to fire AFTER the borrow of `self.state` ends: the emit
        // path borrows `&self` (for the publisher) while the loop holds `&mut
        // self.state` via `entry`, so deltas are queued and flushed post-loop.
        let mut pending: Vec<ClusterEvent> = Vec::new();
        let confirmations = self.config.confirmations;
        for peer in &self.peers {
            let connected = self.liveness.peer_connected(&peer.name);
            let entry = self.state.entry(peer.name.clone()).or_default();
            if connected {
                // RECOVERY EMIT: capture the prior-down signal BEFORE the reset,
                // or every tick would look freshly connected and no recovery
                // event would ever fire.
                let was_down = entry.consecutive_down > 0 || entry.adopted;
                entry.consecutive_down = 0;
                entry.adopted = false;
                if was_down {
                    pending.push(ClusterEvent::PeerConnected {
                        meta: placeholder_meta(),
                        peer_name: peer.name.clone(),
                        forward_addr: None,
                    });
                }
                continue;
            }
            entry.consecutive_down = entry.consecutive_down.saturating_add(1);
            let consecutive_down = entry.consecutive_down;
            let confirmed = consecutive_down >= confirmations;
            // Every tick a peer is observed down is a delta; `confirmed` flips
            // once the debounce threshold authorizes adoption.
            pending.push(ClusterEvent::PeerDisconnected {
                meta: placeholder_meta(),
                peer_name: peer.name.clone(),
                consecutive_down,
                confirmed,
            });
            if entry.adopted || consecutive_down < confirmations {
                continue;
            }
            // Pre-check: skip any of this peer's shards already published to a
            // DIFFERENT live owner — another survivor has adopted them, so racing
            // a second adoption would be wasted work (the fence would drop us
            // anyway). A record naming a peer that is itself down is adoptable (the
            // recorded owner has since died); no record is adoptable too. Mirrors
            // routing::directory::resolve_from_record's down-owner detection.
            if Self::all_shards_handled_elsewhere(
                self.liveness.as_ref(),
                &peer.name,
                &peer.owned_shards,
            ) {
                // Every shard is already served by a live owner: mark handled so
                // the supervisor does NOT retry-loop on shards another node owns.
                entry.adopted = true;
                let held_by = Self::live_owner_of(self.liveness.as_ref(), &peer.owned_shards)
                    .unwrap_or_default();
                pending.push(ClusterEvent::ShardAdoptionSkipped {
                    meta: placeholder_meta(),
                    shards: peer.owned_shards.clone(),
                    from_peer: peer.name.clone(),
                    held_by,
                });
                tracing::info!(
                    peer = %peer.name,
                    shards = ?peer.owned_shards,
                    "downed peer's shards already adopted by another live owner; skipping"
                );
                continue;
            }
            match self.adopter.adopt_shards(&peer.owned_shards).await {
                Ok(()) => {
                    entry.adopted = true;
                    adopted_now.push(peer.name.clone());
                    pending.push(ClusterEvent::ShardAdopted {
                        meta: placeholder_meta(),
                        shards: peer.owned_shards.clone(),
                        from_peer: peer.name.clone(),
                        adopted_by: self.self_node.clone(),
                    });
                    tracing::info!(
                        peer = %peer.name,
                        shards = ?peer.owned_shards,
                        "cluster supervisor adopted a downed peer's shards (SS-5b auto-failover)"
                    );
                }
                Err(error) => {
                    pending.push(ClusterEvent::ShardAdoptionFailed {
                        meta: placeholder_meta(),
                        shards: peer.owned_shards.clone(),
                        from_peer: peer.name.clone(),
                        error: error.clone(),
                    });
                    // Leave `adopted` false so the next tick retries: a
                    // quorum-unavailable / transport adopt error must not strand
                    // the dead peer's shards forever (the retry contract). Note a
                    // fenced (NotOwner) shard is NOT surfaced here — the engine's
                    // clean-partial adopt drops a deposed shard internally and
                    // returns Ok, and the pre-check above already short-circuits a
                    // shard another LIVE owner holds, so this arm is reached only
                    // for genuinely retryable faults.
                    tracing::warn!(
                        peer = %peer.name,
                        shards = ?peer.owned_shards,
                        %error,
                        "cluster supervisor failed to adopt a downed peer's shards; will retry"
                    );
                }
            }
        }
        // Flush queued deltas now that the `&mut self.state` borrow is released:
        // each is re-stamped with a real publisher seq+instant (the placeholder
        // meta is discarded). With no publisher attached this is a no-op.
        for event in pending {
            self.emit(|meta| with_meta(event, meta));
        }
        adopted_now
    }

    /// The live owner currently recorded for the first of `shards` that names a
    /// connected third party, for the `ShardAdoptionSkipped.held_by` field. Reads
    /// only real directory records; returns `None` if none is live-held.
    fn live_owner_of(liveness: &L, shards: &[usize]) -> Option<String> {
        shards.iter().find_map(|&shard| {
            liveness
                .read_shard_owner(shard)
                .filter(|owner| liveness.peer_connected(owner))
        })
    }

    /// Whether EVERY shard in `shards` is already published to a DIFFERENT live
    /// owner — i.e. another survivor has adopted them, so this supervisor has
    /// nothing left to do for the dead `peer_name`. A shard is "handled elsewhere"
    /// only when its directory record names a peer that is BOTH not the dead peer
    /// AND currently connected; a record naming the dead peer (or a peer now down)
    /// or no record at all means the shard is still adoptable. Empty `shards`
    /// is vacuously handled, but such peers are filtered out at construction.
    fn all_shards_handled_elsewhere(liveness: &L, peer_name: &str, shards: &[usize]) -> bool {
        !shards.is_empty()
            && shards.iter().all(|&shard| {
                liveness.read_shard_owner(shard).is_some_and(|owner| {
                    // The recorded owner is a LIVE third party (not the dead peer):
                    // that survivor serves it. A record naming the dead peer itself
                    // is stale (it has since died) and remains adoptable.
                    owner != peer_name && liveness.peer_connected(&owner)
                })
            })
    }

    /// Drive the poll loop until `shutdown` flips true, ticking every
    /// `poll_interval`. Consumes `self`; spawn it as a background task.
    pub async fn run(mut self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        // Lifecycle EMIT: the supervisor is running on this node (ADR-019 calm
        // state distinguishes "running, all healthy" from "not running").
        let self_node = self.self_node.clone();
        self.emit(|meta| ClusterEvent::SupervisorStarted {
            meta,
            node: self_node.clone(),
        });
        let mut interval = tokio::time::interval(self.config.poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    drop(self.tick().await);
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        // Lifecycle EMIT: clean drain/shutdown — the ops console can distinguish a
        // stopped supervisor from "all peers healthy" (ADR-019).
        let self_node = self.self_node.clone();
        self.emit(|meta| ClusterEvent::SupervisorStopped {
            meta,
            node: self_node.clone(),
        });
    }
}

/// A placeholder meta used while a [`ClusterEvent`] is queued inside `tick()`'s
/// `&mut self.state` borrow; it is ALWAYS replaced by the publisher-stamped meta
/// in [`with_meta`] at flush time, so a placeholder seq never reaches the wire.
fn placeholder_meta() -> aion_core::ClusterEventMeta {
    aion_core::ClusterEventMeta {
        cluster_seq: 0,
        observed_at: chrono::Utc::now(),
    }
}

/// Replace a queued event's placeholder meta with the publisher-stamped one.
///
/// The peer/shard topology arms (the only events `tick()` queues with a
/// placeholder) are handled here; worker-lifecycle and the
/// publisher-direct-emit variants delegate to [`with_meta_worker_lifecycle`] to
/// keep each function under the house line limit.
fn with_meta(event: ClusterEvent, meta: aion_core::ClusterEventMeta) -> ClusterEvent {
    match event {
        ClusterEvent::PeerAdded {
            peer_name,
            forward_addr,
            ..
        } => ClusterEvent::PeerAdded {
            meta,
            peer_name,
            forward_addr,
        },
        ClusterEvent::PeerConnected {
            peer_name,
            forward_addr,
            ..
        } => ClusterEvent::PeerConnected {
            meta,
            peer_name,
            forward_addr,
        },
        ClusterEvent::PeerDisconnected {
            peer_name,
            consecutive_down,
            confirmed,
            ..
        } => ClusterEvent::PeerDisconnected {
            meta,
            peer_name,
            consecutive_down,
            confirmed,
        },
        ClusterEvent::ShardAdopted {
            shards,
            from_peer,
            adopted_by,
            ..
        } => ClusterEvent::ShardAdopted {
            meta,
            shards,
            from_peer,
            adopted_by,
        },
        ClusterEvent::ShardAdoptionFailed {
            shards,
            from_peer,
            error,
            ..
        } => ClusterEvent::ShardAdoptionFailed {
            meta,
            shards,
            from_peer,
            error,
        },
        ClusterEvent::ShardAdoptionSkipped {
            shards,
            from_peer,
            held_by,
            ..
        } => ClusterEvent::ShardAdoptionSkipped {
            meta,
            shards,
            from_peer,
            held_by,
        },
        other => with_meta_worker_lifecycle(other, meta),
    }
}

/// Meta re-stamp for the worker-lifecycle, supervisor, and `NamespaceCreated`
/// variants (the tail of [`with_meta`]'s exhaustive match).
///
/// `NamespaceCreated` is emitted directly through the publisher (which stamps the
/// real meta), never queued inside `tick()` with a placeholder, so its arm is
/// unreachable in practice; it re-stamps faithfully to keep the match exhaustive
/// without a wildcard that could silently swallow a future variant. The
/// peer/shard variants never reach here ([`with_meta`] handles them), so they are
/// `unreachable!` rather than silently mis-stamped.
fn with_meta_worker_lifecycle(
    event: ClusterEvent,
    meta: aion_core::ClusterEventMeta,
) -> ClusterEvent {
    match event {
        ClusterEvent::WorkerConnected {
            worker_id,
            namespaces,
            task_queue,
            transport,
            node,
            ..
        } => ClusterEvent::WorkerConnected {
            meta,
            worker_id,
            namespaces,
            task_queue,
            transport,
            node,
        },
        ClusterEvent::WorkerDisconnected {
            worker_id,
            namespaces,
            reason,
            ..
        } => ClusterEvent::WorkerDisconnected {
            meta,
            worker_id,
            namespaces,
            reason,
        },
        ClusterEvent::SupervisorStarted { node, .. } => {
            ClusterEvent::SupervisorStarted { meta, node }
        }
        ClusterEvent::SupervisorStopped { node, .. } => {
            ClusterEvent::SupervisorStopped { meta, node }
        }
        ClusterEvent::NamespaceCreated {
            name,
            created_at,
            origin,
            ..
        } => ClusterEvent::NamespaceCreated {
            meta,
            name,
            created_at,
            origin,
        },
        // Like `NamespaceCreated`, emitted directly through the publisher (which
        // stamps the real meta), never queued in `tick()`; re-stamped faithfully
        // to keep the match exhaustive without a swallowing wildcard.
        ClusterEvent::NamespacePlacementChanged {
            name, placement, ..
        } => ClusterEvent::NamespacePlacementChanged {
            meta,
            name,
            placement,
        },
        ClusterEvent::PeerAdded { .. }
        | ClusterEvent::PeerConnected { .. }
        | ClusterEvent::PeerDisconnected { .. }
        | ClusterEvent::ShardAdopted { .. }
        | ClusterEvent::ShardAdoptionFailed { .. }
        | ClusterEvent::ShardAdoptionSkipped { .. } => {
            unreachable!("peer/shard variants are re-stamped by with_meta, never delegated here")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A liveness fake whose verdict is flipped by the test. `connected` is the
    /// verdict for ALL queried peers EXCEPT names explicitly registered as live
    /// third-party owners via `set_live_owner`, which always report connected and
    /// can be recorded as a shard's owner via `publish`.
    struct FakeLiveness {
        connected: AtomicBool,
        /// shard -> recorded owner name (the SS-3 directory record).
        owners: Mutex<std::collections::BTreeMap<usize, String>>,
        /// peer names that always report connected (live third-party survivors).
        live_owners: Mutex<std::collections::BTreeSet<String>>,
    }

    impl FakeLiveness {
        fn new(connected: bool) -> Self {
            Self {
                connected: AtomicBool::new(connected),
                owners: Mutex::new(std::collections::BTreeMap::new()),
                live_owners: Mutex::new(std::collections::BTreeSet::new()),
            }
        }
        fn set(&self, connected: bool) {
            self.connected.store(connected, Ordering::SeqCst);
        }
        /// Record `owner` as `shard`'s directory owner and (if `live`) mark it as
        /// a connected third-party survivor.
        fn publish(&self, shard: usize, owner: &str, live: bool) {
            self.owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(shard, owner.to_owned());
            if live {
                self.live_owners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(owner.to_owned());
            }
        }
    }

    impl PeerLiveness for FakeLiveness {
        fn peer_connected(&self, peer_name: &str) -> bool {
            if self
                .live_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(peer_name)
            {
                return true;
            }
            self.connected.load(Ordering::SeqCst)
        }

        fn read_shard_owner(&self, shard: usize) -> Option<String> {
            self.owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&shard)
                .cloned()
        }
    }

    /// An adopter fake recording every adopt call, optionally failing the first.
    struct FakeAdopter {
        calls: Mutex<Vec<Vec<usize>>>,
        fail_first: AtomicBool,
    }

    impl FakeAdopter {
        fn new(fail_first: bool) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_first: AtomicBool::new(fail_first),
            }
        }
        fn calls(&self) -> Vec<Vec<usize>> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    #[async_trait::async_trait]
    impl ShardAdopter for FakeAdopter {
        async fn adopt_shards(&self, shards: &[usize]) -> Result<(), String> {
            if self.fail_first.swap(false, Ordering::SeqCst) {
                return Err("simulated election failure".to_owned());
            }
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(shards.to_vec());
            Ok(())
        }
    }

    fn supervisor(
        liveness: Arc<FakeLiveness>,
        adopter: Arc<FakeAdopter>,
        confirmations: u32,
    ) -> ClusterSupervisor<FakeLiveness, FakeAdopter> {
        ClusterSupervisor::new(
            liveness,
            adopter,
            vec![WatchedPeer {
                name: "node-1@127.0.0.1".to_owned(),
                owned_shards: vec![1],
            }],
            SupervisorConfig {
                poll_interval: Duration::from_millis(1),
                confirmations,
            },
        )
    }

    #[tokio::test]
    async fn does_not_adopt_while_peer_connected() {
        let liveness = Arc::new(FakeLiveness::new(true));
        let adopter = Arc::new(FakeAdopter::new(false));
        let mut sup = supervisor(Arc::clone(&liveness), Arc::clone(&adopter), 2);
        for _ in 0..5 {
            assert!(sup.tick().await.is_empty());
        }
        assert!(adopter.calls().is_empty(), "no adoption while peer is up");
    }

    #[tokio::test]
    async fn debounce_requires_consecutive_down_before_adopting() {
        let liveness = Arc::new(FakeLiveness::new(true));
        let adopter = Arc::new(FakeAdopter::new(false));
        let mut sup = supervisor(Arc::clone(&liveness), Arc::clone(&adopter), 3);

        liveness.set(false);
        assert!(sup.tick().await.is_empty(), "tick 1 down: below threshold");
        // A blip back up resets the counter.
        liveness.set(true);
        assert!(sup.tick().await.is_empty());
        liveness.set(false);
        assert!(
            sup.tick().await.is_empty(),
            "down again, counter reset to 1"
        );
        assert!(sup.tick().await.is_empty(), "2 consecutive: still below 3");
        let fired = sup.tick().await;
        assert_eq!(
            fired,
            vec!["node-1@127.0.0.1".to_owned()],
            "3rd consecutive triggers"
        );
        assert_eq!(adopter.calls(), vec![vec![1]]);
    }

    #[tokio::test]
    async fn adopts_once_then_stays_quiet_while_down() {
        let liveness = Arc::new(FakeLiveness::new(false));
        let adopter = Arc::new(FakeAdopter::new(false));
        let mut sup = supervisor(Arc::clone(&liveness), Arc::clone(&adopter), 1);
        assert_eq!(sup.tick().await.len(), 1, "first down tick adopts");
        for _ in 0..5 {
            assert!(sup.tick().await.is_empty(), "no re-adopt while still down");
        }
        assert_eq!(adopter.calls(), vec![vec![1]], "adopted exactly once");
    }

    #[tokio::test]
    async fn failed_adoption_is_retried_next_tick() {
        let liveness = Arc::new(FakeLiveness::new(false));
        let adopter = Arc::new(FakeAdopter::new(true)); // first adopt fails
        let mut sup = supervisor(Arc::clone(&liveness), Arc::clone(&adopter), 1);
        assert!(
            sup.tick().await.is_empty(),
            "first adopt fails, not recorded"
        );
        assert!(adopter.calls().is_empty());
        assert_eq!(sup.tick().await.len(), 1, "retry succeeds next tick");
        assert_eq!(adopter.calls(), vec![vec![1]]);
    }

    #[tokio::test]
    async fn peer_with_no_shards_is_not_watched() {
        let liveness = Arc::new(FakeLiveness::new(false));
        let adopter = Arc::new(FakeAdopter::new(false));
        let mut sup = ClusterSupervisor::new(
            Arc::clone(&liveness),
            Arc::clone(&adopter),
            vec![WatchedPeer {
                name: "node-2@127.0.0.1".to_owned(),
                owned_shards: vec![],
            }],
            SupervisorConfig {
                poll_interval: Duration::from_millis(1),
                confirmations: 1,
            },
        );
        assert!(!sup.watches_any());
        assert!(sup.tick().await.is_empty());
        assert!(adopter.calls().is_empty());
    }

    /// PRE-CHECK: a downed peer whose shard is ALREADY published to a DIFFERENT
    /// LIVE owner is NOT adopted — another survivor holds it. The supervisor marks
    /// the peer handled (no retry-loop) and never calls the adopter.
    #[tokio::test]
    async fn shard_already_published_to_live_owner_is_not_adopted() {
        let liveness = Arc::new(FakeLiveness::new(false));
        // Shard 1 (the watched peer's shard) is recorded as owned by a live third
        // party, node-9 — it adopted the shard already.
        liveness.publish(1, "node-9@127.0.0.1", true);
        let adopter = Arc::new(FakeAdopter::new(false));
        let mut sup = supervisor(Arc::clone(&liveness), Arc::clone(&adopter), 1);

        // The peer is down past the threshold, but its shard is handled elsewhere.
        assert!(
            sup.tick().await.is_empty(),
            "no adoption fires for a shard a live owner already holds"
        );
        assert!(
            adopter.calls().is_empty(),
            "the adopter is never invoked for an already-handled shard"
        );
        // Subsequent ticks stay quiet: marked handled, no retry-loop.
        for _ in 0..3 {
            assert!(sup.tick().await.is_empty());
        }
        assert!(adopter.calls().is_empty());
    }

    /// A directory record naming a peer that is itself DOWN is NOT "handled
    /// elsewhere": the recorded owner has since died, so the shard remains
    /// adoptable and the supervisor adopts it.
    #[tokio::test]
    async fn shard_published_to_a_down_owner_is_still_adopted() {
        let liveness = Arc::new(FakeLiveness::new(false));
        // Shard 1 recorded as owned by node-9, but node-9 is NOT live (not
        // registered as a live owner) — `connected=false` applies to it.
        liveness.publish(1, "node-9@127.0.0.1", false);
        let adopter = Arc::new(FakeAdopter::new(false));
        let mut sup = supervisor(Arc::clone(&liveness), Arc::clone(&adopter), 1);

        assert_eq!(
            sup.tick().await.len(),
            1,
            "a shard whose recorded owner is itself down is adoptable"
        );
        assert_eq!(adopter.calls(), vec![vec![1]]);
    }

    /// WS3 EMIT: with a publisher attached, a down tick emits `PeerDisconnected`
    /// (confirmed flipping at the threshold) and the adoption tick emits
    /// `ShardAdopted` carrying this node's name. The recovery EMIT then fires
    /// `PeerConnected` — proving the capture-before-reset: a freshly-reconnected
    /// peer that was previously down/adopted yields exactly one recovery event.
    #[tokio::test]
    async fn tick_emits_topology_deltas_through_the_publisher()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::num::NonZeroUsize;

        use aion_core::ClusterEvent;
        use futures::StreamExt;

        use crate::cluster_publisher::ClusterEventPublisher;

        let capacity = NonZeroUsize::new(64).ok_or("non-zero")?;
        let publisher = Arc::new(ClusterEventPublisher::new(capacity));
        let mut subscription = publisher.subscribe(0);

        let liveness = Arc::new(FakeLiveness::new(true));
        let adopter = Arc::new(FakeAdopter::new(false));
        let mut sup = supervisor(Arc::clone(&liveness), Arc::clone(&adopter), 2)
            .with_publisher(Arc::clone(&publisher), "node-self@127.0.0.1");

        // Tick 1 down: PeerDisconnected{confirmed=false} (below threshold 2).
        liveness.set(false);
        drop(sup.tick().await);
        // Tick 2 down: PeerDisconnected{confirmed=true} then ShardAdopted.
        let fired = sup.tick().await;
        assert_eq!(fired, vec!["node-1@127.0.0.1".to_owned()]);

        // Drain the three emitted deltas in order.
        let first = next_event(&mut subscription).await?;
        assert!(
            matches!(
                &first,
                ClusterEvent::PeerDisconnected {
                    confirmed: false,
                    consecutive_down: 1,
                    ..
                }
            ),
            "first delta must be an unconfirmed down: {first:?}"
        );
        let second = next_event(&mut subscription).await?;
        assert!(
            matches!(
                &second,
                ClusterEvent::PeerDisconnected {
                    confirmed: true,
                    consecutive_down: 2,
                    ..
                }
            ),
            "second delta must be the confirmed down: {second:?}"
        );
        let third = next_event(&mut subscription).await?;
        let ClusterEvent::ShardAdopted {
            shards,
            adopted_by,
            from_peer,
            ..
        } = &third
        else {
            return Err(format!("third delta must be ShardAdopted: {third:?}").into());
        };
        assert_eq!(shards, &vec![1]);
        assert_eq!(adopted_by, "node-self@127.0.0.1");
        assert_eq!(from_peer, "node-1@127.0.0.1");

        // RECOVERY: peer comes back up. The capture-before-reset must fire exactly
        // one PeerConnected for the now-recovered (previously adopted) peer.
        liveness.set(true);
        drop(sup.tick().await);
        let recovery = next_event(&mut subscription).await?;
        assert!(
            matches!(&recovery, ClusterEvent::PeerConnected { .. }),
            "recovery delta must be PeerConnected: {recovery:?}"
        );

        // A second connected tick (already reset) must NOT re-emit recovery: the
        // next delta is whatever a subsequent down produces, never a duplicate
        // PeerConnected. Quiet tick yields nothing.
        let quiet = sup.tick().await;
        assert!(quiet.is_empty());
        // No further event is buffered (no spurious recovery re-emit).
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), subscription.next())
                .await
                .is_err(),
            "a steady connected peer must not re-emit PeerConnected every tick"
        );
        Ok(())
    }

    async fn next_event(
        subscription: &mut futures::stream::BoxStream<
            'static,
            Result<aion_core::ClusterEvent, crate::cluster_publisher::ClusterStreamLagged>,
        >,
    ) -> Result<aion_core::ClusterEvent, Box<dyn std::error::Error>> {
        use futures::StreamExt;
        tokio::time::timeout(std::time::Duration::from_secs(1), subscription.next())
            .await?
            .ok_or("cluster subscription ended")?
            .map_err(|lag| format!("unexpected lag: {lag:?}").into())
    }

    /// A record naming the DEAD peer itself (the steady-state declared owner) is
    /// stale and does NOT block adoption.
    #[tokio::test]
    async fn shard_published_to_the_dead_peer_itself_is_adopted() {
        let liveness = Arc::new(FakeLiveness::new(false));
        // The directory still names the (now dead) declared owner of shard 1.
        liveness.publish(1, "node-1@127.0.0.1", false);
        let adopter = Arc::new(FakeAdopter::new(false));
        let mut sup = supervisor(Arc::clone(&liveness), Arc::clone(&adopter), 1);

        assert_eq!(
            sup.tick().await.len(),
            1,
            "a record naming the dead peer itself is stale and still adoptable"
        );
        assert_eq!(adopter.calls(), vec![vec![1]]);
    }
}
