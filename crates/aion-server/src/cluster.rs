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

/// The liveness signal the supervisor polls. Implemented by [`HaematiteStore`]
/// in production and by a fake in tests, so the debounce/adopt logic is verified
/// without standing up a real cluster every time.
pub trait PeerLiveness: Send + Sync + 'static {
    /// Whether the peer named `peer_name` currently holds a live replication link.
    fn peer_connected(&self, peer_name: &str) -> bool;
}

#[cfg(feature = "haematite-backend")]
impl PeerLiveness for aion_store_haematite::HaematiteStore {
    fn peer_connected(&self, peer_name: &str) -> bool {
        Self::peer_connected(self, peer_name)
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
        for peer in &self.peers {
            let connected = self.liveness.peer_connected(&peer.name);
            let entry = self.state.entry(peer.name.clone()).or_default();
            if connected {
                entry.consecutive_down = 0;
                entry.adopted = false;
                continue;
            }
            entry.consecutive_down = entry.consecutive_down.saturating_add(1);
            if entry.adopted || entry.consecutive_down < self.config.confirmations {
                continue;
            }
            match self.adopter.adopt_shards(&peer.owned_shards).await {
                Ok(()) => {
                    entry.adopted = true;
                    adopted_now.push(peer.name.clone());
                    tracing::info!(
                        peer = %peer.name,
                        shards = ?peer.owned_shards,
                        "cluster supervisor adopted a downed peer's shards (SS-5b auto-failover)"
                    );
                }
                Err(error) => {
                    // Leave `adopted` false so the next tick retries: a failed
                    // election (e.g. quorum not yet reachable) must not strand the
                    // dead peer's shards forever.
                    tracing::warn!(
                        peer = %peer.name,
                        shards = ?peer.owned_shards,
                        %error,
                        "cluster supervisor failed to adopt a downed peer's shards; will retry"
                    );
                }
            }
        }
        adopted_now
    }

    /// Drive the poll loop until `shutdown` flips true, ticking every
    /// `poll_interval`. Consumes `self`; spawn it as a background task.
    pub async fn run(mut self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
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
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A liveness fake whose verdict is flipped by the test.
    struct FakeLiveness {
        connected: AtomicBool,
    }

    impl FakeLiveness {
        fn new(connected: bool) -> Self {
            Self {
                connected: AtomicBool::new(connected),
            }
        }
        fn set(&self, connected: bool) {
            self.connected.store(connected, Ordering::SeqCst);
        }
    }

    impl PeerLiveness for FakeLiveness {
        fn peer_connected(&self, _peer_name: &str) -> bool {
            self.connected.load(Ordering::SeqCst)
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
}
