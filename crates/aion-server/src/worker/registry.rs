//! Connected-worker registry keyed by worker-pool address and activity type.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use aion_core::{ClusterEvent, WorkerDeathReason, WorkerTransport};
use aion_proto::{ProtoActivityTask, ProtoRegisterWorker};
use tokio::sync::{Notify, mpsc};

use crate::cluster_publisher::ClusterEventPublisher;
use crate::error::ServerError;
use crate::namespace::{CallerIdentity, NamespaceGuard, NamespaceOperation};
use crate::observability::Metrics;

/// The literal task queue an empty/absent selector normalizes to.
///
/// A worker-pool address has two disjoint dimensions; the second one
/// (`task_queue`) is a liveness selector, not a correctness boundary. An empty
/// `task_queue` is normalized to this one named default pool so a producer that
/// names no queue and a worker that advertises none both land on the same pool.
///
/// Re-exported from [`aion_core::DEFAULT_TASK_QUEUE`] so the server cannot drift
/// from the canonical domain default; the name is kept stable here for existing
/// call sites.
pub use aion_core::DEFAULT_TASK_QUEUE;

/// Server-side handle used to push activity tasks to a connected worker stream.
pub type WorkerTaskSender = mpsc::Sender<WorkerMessage>;

/// Transport through which the server delivers a dispatch to a registered worker.
///
/// A worker is selected the SAME way regardless of transport (`select_worker`
/// over the `(namespace, task_queue, node)` pool key); only the delivery leg
/// differs. The default gRPC path pushes a [`WorkerMessage`] onto the worker's
/// stream `mpsc` ([`WorkerDelivery::Grpc`]); a liminal-connected worker is
/// delivered to by pushing the dispatch out on its existing liminal connection
/// ([`WorkerDelivery::Liminal`], feature-gated). This enum is the minimal
/// transport-agnostic seam: the registry holds it on each [`WorkerHandle`], and
/// the dispatch path reads the variant it needs. The gRPC variant carries exactly
/// the `mpsc::Sender` it always did, so the gRPC dispatch path is unchanged.
#[derive(Clone, Debug)]
pub enum WorkerDelivery {
    /// gRPC stream delivery: the dispatch path pushes a [`WorkerMessage`] onto
    /// this `mpsc` sender, exactly as before this enum existed.
    Grpc(WorkerTaskSender),
    /// Liminal server-push delivery: the dispatch path pushes the serialized
    /// dispatch out on the worker's existing liminal connection and awaits the
    /// correlated reply. Carries the connection identity needed to address that
    /// push.
    #[cfg(feature = "liminal-transport")]
    Liminal(crate::worker::liminal_transport::LiminalWorkerDelivery),
}

/// Message queued from server-side dispatch/shutdown into a worker stream writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerMessage {
    /// Activity invocation pushed to a worker.
    ActivityTask(ProtoActivityTask),
    /// Graceful-shutdown notification; no new work will be dispatched.
    DrainRequest,
}

/// Address of a worker pool: the two disjoint routing dimensions that select a
/// pool, before an `activity_type` is matched within it.
///
/// `namespace` is the correctness/isolation boundary — a workflow's activities
/// only ever reach workers in the workflow's namespace, so crossing it is a bug.
/// `task_queue` is the pool/flavour selector within that namespace (norn /
/// claude / cpu / gpu) — a miss is a liveness issue, never a correctness one.
///
/// This is a named type rather than a `(String, String)` tuple so a `node`
/// dimension (Tier 3 affinity) can be added later without re-threading every
/// call site.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PoolAddress {
    namespace: String,
    task_queue: String,
}

impl PoolAddress {
    /// Build a pool address, normalizing an empty `task_queue` to the named
    /// [`DEFAULT_TASK_QUEUE`] pool. The `namespace` is the authorization
    /// boundary and is never normalized.
    #[must_use]
    pub fn new(namespace: impl Into<String>, task_queue: impl Into<String>) -> Self {
        let task_queue = task_queue.into();
        let task_queue = if task_queue.is_empty() {
            String::from(DEFAULT_TASK_QUEUE)
        } else {
            task_queue
        };
        Self {
            namespace: namespace.into(),
            task_queue,
        }
    }

    /// The correctness/isolation boundary of this pool.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The pool/flavour selector within the namespace.
    #[must_use]
    pub fn task_queue(&self) -> &str {
        &self.task_queue
    }
}

/// Registry match key: a worker-pool address plus the activity type matched
/// within that pool. A named type (not an anonymous tuple) so the routing
/// identity stays self-describing and extensible.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct ActivityKey {
    pool: PoolAddress,
    activity_type: String,
}

impl ActivityKey {
    fn new(pool: PoolAddress, activity_type: impl Into<String>) -> Self {
        Self {
            pool,
            activity_type: activity_type.into(),
        }
    }
}

type WorkerMap = HashMap<WorkerId, WorkerHandle>;
type RegistryMap = HashMap<ActivityKey, WorkerMap>;

/// Stable identifier assigned to a connected worker stream.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkerId(u64);

impl WorkerId {
    /// Raw numeric value, as carried by the wire `RegisterAck.worker_id` so
    /// workers can correlate their logs with the server's.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Cloneable handle for a registered worker stream.
///
/// A worker serves a SET of namespaces under a single `task_queue`, so it is
/// indexed under one `(namespace, task_queue, activity_type)` key per namespace
/// in its set. `node` is an OPTIONAL locality affinity (a locality, not a
/// process — many handles may share a node id) used as a within-pool filter at
/// selection time; `None` means the worker advertised no locality.
#[derive(Clone, Debug)]
pub struct WorkerHandle {
    id: WorkerId,
    namespaces: BTreeSet<String>,
    task_queue: String,
    node: Option<String>,
    activity_types: BTreeSet<String>,
    delivery: WorkerDelivery,
}

impl WorkerHandle {
    /// Worker identifier assigned by this server process.
    #[must_use]
    pub const fn id(&self) -> WorkerId {
        self.id
    }

    /// Namespaces authorized for this worker stream. The worker is reachable for
    /// a dispatch only when its set includes the workflow's namespace.
    #[must_use]
    pub const fn namespaces(&self) -> &BTreeSet<String> {
        &self.namespaces
    }

    /// Task queue (pool/flavour) this worker serves within each namespace.
    #[must_use]
    pub fn task_queue(&self) -> &str {
        &self.task_queue
    }

    /// Optional locality affinity this worker advertised. `None` means the
    /// worker carries no node and is reachable only for unpinned dispatches.
    #[must_use]
    pub fn node(&self) -> Option<&str> {
        self.node.as_deref()
    }

    /// Activity types advertised by this worker.
    #[must_use]
    pub fn activity_types(&self) -> &BTreeSet<String> {
        &self.activity_types
    }

    /// The transport this worker is delivered to through.
    #[must_use]
    pub const fn delivery(&self) -> &WorkerDelivery {
        &self.delivery
    }

    /// gRPC stream sender used by the gRPC dispatch path to push work, or `None`
    /// when this worker is delivered to over a non-gRPC transport (liminal).
    ///
    /// The gRPC dispatch path registers every worker with a [`WorkerDelivery::Grpc`]
    /// delivery, so this is always `Some` for a gRPC-registered worker — the
    /// behaviour the path relied on before delivery became transport-agnostic.
    #[must_use]
    pub fn sender(&self) -> Option<&WorkerTaskSender> {
        match &self.delivery {
            WorkerDelivery::Grpc(sender) => Some(sender),
            #[cfg(feature = "liminal-transport")]
            WorkerDelivery::Liminal(_) => None,
        }
    }
}

#[derive(Debug)]
struct RegistryState {
    next_worker_id: u64,
    workers: BTreeMap<WorkerId, WorkerHandle>,
    by_activity: RegistryMap,
    /// Round-robin cursor per `(namespace, task_queue, activity_type)` triple, so
    /// each pool rotates independently of every other pool.
    rotation: HashMap<ActivityKey, usize>,
}

impl Default for RegistryState {
    fn default() -> Self {
        Self {
            next_worker_id: 1,
            workers: BTreeMap::new(),
            by_activity: HashMap::new(),
            rotation: HashMap::new(),
        }
    }
}

/// Cloneable registry of currently connected worker streams.
#[derive(Clone, Debug)]
pub struct ConnectedWorkerRegistry {
    inner: Arc<Mutex<RegistryState>>,
    metrics: Option<Metrics>,
    /// WS3 cluster-event publisher: emits `WorkerConnected`/`WorkerDisconnected`
    /// topology deltas on register/deregister. `None` keeps existing
    /// constructions (and every test) silent, exactly like `metrics`.
    cluster_publisher: Option<ClusterEventPublisher>,
    worker_arrived: Arc<Notify>,
}

impl Default for ConnectedWorkerRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState::default())),
            metrics: None,
            cluster_publisher: None,
            worker_arrived: Arc::new(Notify::new()),
        }
    }
}

impl ConnectedWorkerRegistry {
    /// Build a registry that records connected-worker gauge updates.
    #[must_use]
    pub fn with_metrics(metrics: Metrics) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState::default())),
            metrics: Some(metrics),
            cluster_publisher: None,
            worker_arrived: Arc::new(Notify::new()),
        }
    }

    /// Attach the WS3 cluster-event publisher so worker topology changes are
    /// pushed to the dashboard. Pure builder addition.
    #[must_use]
    pub fn with_cluster_publisher(mut self, publisher: ClusterEventPublisher) -> Self {
        self.cluster_publisher = Some(publisher);
        self
    }

    /// Authorize a worker registration and insert it into the connected-worker registry.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] if namespace authorization fails or the registry lock is poisoned.
    pub async fn accept_registration(
        &self,
        guard: &NamespaceGuard,
        caller: &CallerIdentity,
        registration: &ProtoRegisterWorker,
        sender: WorkerTaskSender,
    ) -> Result<WorkerRegistration, ServerError> {
        // Verify the operation against the guard's worker-registration policy,
        // then authorize EACH namespace in the worker's set: a worker serves a
        // SET of correctness boundaries, so the registration is denied unless
        // the caller is granted every one. The wire's empty `node` carries no
        // locality affinity; a non-empty value is the worker's advertised node.
        guard
            .scope(caller, &NamespaceOperation::register_worker(registration))
            .await?;
        let namespaces = guard.scope_worker_namespaces(caller, &registration.namespaces)?;
        let node = optional_node(&registration.node);
        self.register_namespaces(
            namespaces,
            registration.task_queue.clone(),
            node,
            registration.activity_types.iter(),
            sender,
        )
    }

    /// Insert an already-authorized worker stream into the default task queue of
    /// a single `namespace`, with no node affinity.
    ///
    /// Convenience over [`Self::register_namespaces`] for callers that serve one
    /// namespace and do not select a task queue (notably tests of the default
    /// pool).
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn register<'a>(
        &self,
        namespace: impl Into<String>,
        activity_types: impl IntoIterator<Item = &'a String>,
        sender: WorkerTaskSender,
    ) -> Result<WorkerRegistration, ServerError> {
        self.register_namespaces(
            [namespace.into()],
            String::from(DEFAULT_TASK_QUEUE),
            None,
            activity_types,
            sender,
        )
    }

    /// Insert an already-authorized worker stream into one explicit worker pool
    /// (single namespace + task queue), with no node affinity.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn register_pool<'a>(
        &self,
        pool: PoolAddress,
        activity_types: impl IntoIterator<Item = &'a String>,
        sender: WorkerTaskSender,
    ) -> Result<WorkerRegistration, ServerError> {
        let PoolAddress {
            namespace,
            task_queue,
        } = pool;
        self.register_namespaces([namespace], task_queue, None, activity_types, sender)
    }

    /// Insert an already-authorized worker stream serving a SET of namespaces
    /// under one `task_queue`, with an optional `node` locality affinity.
    ///
    /// The worker is indexed under one `(namespace, task_queue, activity_type)`
    /// key per namespace in its set, so a dispatch in any of those namespaces
    /// can reach it. `node` is recorded on the handle and used only as a
    /// within-pool filter at selection time — it is NOT part of [`PoolAddress`].
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn register_namespaces<'a>(
        &self,
        namespaces: impl IntoIterator<Item = String>,
        task_queue: impl Into<String>,
        node: Option<String>,
        activity_types: impl IntoIterator<Item = &'a String>,
        sender: WorkerTaskSender,
    ) -> Result<WorkerRegistration, ServerError> {
        self.register_delivery(
            namespaces,
            task_queue,
            node,
            activity_types,
            WorkerDelivery::Grpc(sender),
        )
    }

    /// Insert an already-authorized worker serving a SET of namespaces under one
    /// `task_queue` and optional `node`, delivered to through an explicit
    /// [`WorkerDelivery`] transport.
    ///
    /// This is the transport-agnostic registration core: [`Self::register_namespaces`]
    /// is the gRPC façade over it (it wraps the stream sender in
    /// [`WorkerDelivery::Grpc`]). Selection (`select_worker`/`workers_for`) is
    /// identical across transports; only the held delivery differs.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn register_delivery<'a>(
        &self,
        namespaces: impl IntoIterator<Item = String>,
        task_queue: impl Into<String>,
        node: Option<String>,
        activity_types: impl IntoIterator<Item = &'a String>,
        delivery: WorkerDelivery,
    ) -> Result<WorkerRegistration, ServerError> {
        let namespaces = namespaces.into_iter().collect::<BTreeSet<_>>();
        let task_queue = task_queue.into();
        let activity_types = activity_types.into_iter().cloned().collect::<BTreeSet<_>>();
        let mut state = self.state()?;
        let worker_id = WorkerId(state.next_worker_id);
        state.next_worker_id = state.next_worker_id.saturating_add(1);

        // Capture the node affinity for the WS3 WorkerConnected delta before the
        // handle moves it.
        let node_for_event = node.clone();
        let handle = WorkerHandle {
            id: worker_id,
            namespaces: namespaces.clone(),
            task_queue: task_queue.clone(),
            node,
            activity_types: activity_types.clone(),
            delivery,
        };

        for namespace in &namespaces {
            let pool = PoolAddress::new(namespace.clone(), task_queue.clone());
            for activity_type in &activity_types {
                state
                    .by_activity
                    .entry(ActivityKey::new(pool.clone(), activity_type.clone()))
                    .or_default()
                    .insert(worker_id, handle.clone());
            }
        }
        let transport = transport_of(&handle.delivery);
        state.workers.insert(worker_id, handle);
        drop(state);

        if let Some(metrics) = &self.metrics {
            for namespace in &namespaces {
                metrics.worker_connected(namespace);
            }
        }

        // WS3: one WorkerConnected delta carrying the full namespace set (the
        // event is namespace-list-valued; the deploy-scoped cluster channel sees
        // it whole). Edge-triggered by the real insert, never a poll.
        if let Some(publisher) = &self.cluster_publisher {
            let namespaces_vec: Vec<String> = namespaces.iter().cloned().collect();
            let task_queue_owned = task_queue.clone();
            drop(publisher.emit(|meta| ClusterEvent::WorkerConnected {
                meta,
                worker_id: worker_id.value().to_string(),
                namespaces: namespaces_vec,
                task_queue: task_queue_owned,
                transport,
                node: node_for_event,
            }));
        }

        self.worker_arrived.notify_waiters();

        Ok(WorkerRegistration {
            registry: self.clone(),
            parts: Some(WorkerRegistrationParts {
                worker_id,
                namespaces,
                task_queue,
                activity_types,
            }),
        })
    }

    /// Wait until at least one new worker registers.
    ///
    /// Returns immediately if a registration occurred since the last call.
    /// Callers should re-check the registry after waking — the newly arrived
    /// worker may not serve the namespace or activity type the caller needs.
    pub async fn wait_for_worker(&self) {
        self.worker_arrived.notified().await;
    }

    /// Return a snapshot of workers registered for the
    /// `(namespace, task_queue, activity_type)` pool, ordered by worker id and
    /// then rotated so each call starts from the next worker in the pool. The
    /// rotation cursor is per triple, so each pool round-robins independently.
    ///
    /// When `node` is `Some`, the result is filtered to workers whose advertised
    /// node equals it — a dispatch pinned to a node reaches only workers on that
    /// node (NODE affinity = require). When `node` is `None`, the behaviour is
    /// exactly the unpinned pool: every worker in the `(namespace, task_queue)`
    /// pool is a candidate regardless of locality. node is a within-pool filter,
    /// NOT part of the pool key, so the per-triple rotation cursor is shared
    /// across pinned and unpinned lookups of the same pool.
    ///
    /// The id sort matters: `by_activity` holds workers in a `HashMap`, whose
    /// iteration order is unspecified. Sorting first makes the rotation below
    /// the sole, deterministic source of ordering — true round-robin across
    /// calls with the same membership, not a wobble layered on hash order.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn workers_for(
        &self,
        namespace: &str,
        task_queue: &str,
        activity_type: &str,
        node: Option<&str>,
    ) -> Result<Vec<WorkerHandle>, ServerError> {
        let mut state = self.state()?;
        let key = ActivityKey::new(PoolAddress::new(namespace, task_queue), activity_type);
        let mut workers: Vec<WorkerHandle> = state
            .by_activity
            .get(&key)
            .map(|workers| {
                workers
                    .values()
                    .filter(|worker| worker_matches_node(worker, node))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if workers.is_empty() {
            return Ok(workers);
        }
        workers.sort_by_key(WorkerHandle::id);
        let idx = state.rotation.entry(key).or_insert(0);
        let start = *idx % workers.len();
        *idx = idx.wrapping_add(1);
        let mut rotated = Vec::with_capacity(workers.len());
        rotated.extend_from_slice(&workers[start..]);
        rotated.extend_from_slice(&workers[..start]);
        Ok(rotated)
    }

    /// Return a snapshot of every connected worker stream.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn all_workers(&self) -> Result<Vec<WorkerHandle>, ServerError> {
        let state = self.state()?;
        Ok(state.workers.values().cloned().collect())
    }

    /// Broadcast a graceful drain request to every connected worker stream.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn broadcast_drain(&self) -> Result<usize, ServerError> {
        let workers = self.all_workers()?;
        let mut delivered = 0usize;
        for worker in workers {
            // Only gRPC-stream workers carry a drain mpsc. A worker on a non-gRPC
            // transport (liminal) has no drain frame in this spike, so it is left
            // untouched rather than spuriously deregistered.
            let Some(sender) = worker.sender() else {
                continue;
            };
            if sender.try_send(WorkerMessage::DrainRequest).is_ok() {
                delivered = delivered.saturating_add(1);
            } else {
                self.deregister(worker.id())?;
            }
        }
        Ok(delivered)
    }

    /// Select one worker for the `(namespace, task_queue, activity_type)` pool.
    ///
    /// When `node` is `Some`, only workers whose advertised node equals it are
    /// considered (NODE affinity = require); `None` considers every worker in
    /// the pool. node is a within-pool filter, NOT part of the pool key.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn select_worker(
        &self,
        namespace: &str,
        task_queue: &str,
        activity_type: &str,
        node: Option<&str>,
    ) -> Result<Option<WorkerHandle>, ServerError> {
        let state = self.state()?;
        let key = ActivityKey::new(PoolAddress::new(namespace, task_queue), activity_type);
        Ok(state.by_activity.get(&key).and_then(|workers| {
            workers
                .values()
                .filter(|worker| worker_matches_node(worker, node))
                .min_by_key(|worker| worker.id)
                .cloned()
        }))
    }

    /// Return whether a worker stream is currently registered.
    ///
    /// The activity dispatch path uses this after queuing a task to detect a
    /// worker whose stream tore down concurrently: a sweep that ran before
    /// the dispatch tracked its task can never complete it, so the dispatch
    /// must fail the activity itself instead of waiting forever.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn is_registered(&self, worker_id: WorkerId) -> Result<bool, ServerError> {
        Ok(self.state()?.workers.contains_key(&worker_id))
    }

    /// Remove a worker by id from every namespace/activity index it advertised.
    ///
    /// Emits a WS3 [`WorkerDeathReason::Disconnect`] delta — the truthful default
    /// for a removed worker whose stream/registration went away. Callers that can
    /// PROVE a finer reason (a liveness-timeout sweep) call
    /// [`Self::deregister_with_reason`] instead, so the dashboard never sees a
    /// fabricated distinction.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn deregister(&self, worker_id: WorkerId) -> Result<(), ServerError> {
        self.deregister_with_reason(worker_id, WorkerDeathReason::Disconnect)
    }

    /// Remove a worker by id, attributing the departure to an explicit
    /// [`WorkerDeathReason`] the caller can prove at its call site (for example a
    /// heartbeat sweep passes [`WorkerDeathReason::Timeout`]).
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn deregister_with_reason(
        &self,
        worker_id: WorkerId,
        reason: WorkerDeathReason,
    ) -> Result<(), ServerError> {
        let mut state = self.state()?;
        let removed_namespaces = Self::remove_worker(&mut state, worker_id);
        drop(state);

        let Some(namespaces) = removed_namespaces else {
            // Already gone: no metrics double-count, no duplicate delta.
            return Ok(());
        };

        if let Some(metrics) = &self.metrics {
            for namespace in &namespaces {
                metrics.worker_disconnected(namespace);
            }
        }
        self.emit_worker_disconnected(worker_id, &namespaces, reason);

        Ok(())
    }

    /// Emit a WS3 `WorkerDisconnected` delta if a publisher is attached.
    fn emit_worker_disconnected(
        &self,
        worker_id: WorkerId,
        namespaces: &BTreeSet<String>,
        reason: WorkerDeathReason,
    ) {
        if let Some(publisher) = &self.cluster_publisher {
            let namespaces_vec: Vec<String> = namespaces.iter().cloned().collect();
            drop(publisher.emit(|meta| ClusterEvent::WorkerDisconnected {
                meta,
                worker_id: worker_id.value().to_string(),
                namespaces: namespaces_vec,
                reason,
            }));
        }
    }

    /// Remove a worker from every `(namespace, task_queue, activity_type)` index
    /// it advertised. Returns the namespace set it served (for metrics), or
    /// `None` if the worker was already gone.
    fn remove_worker(state: &mut RegistryState, worker_id: WorkerId) -> Option<BTreeSet<String>> {
        let handle = state.workers.remove(&worker_id)?;

        for namespace in &handle.namespaces {
            let pool = PoolAddress::new(namespace.clone(), handle.task_queue.clone());
            for activity_type in &handle.activity_types {
                let key = ActivityKey::new(pool.clone(), activity_type.clone());
                if let Some(workers) = state.by_activity.get_mut(&key) {
                    workers.remove(&worker_id);
                    if workers.is_empty() {
                        state.by_activity.remove(&key);
                        // Prune the round-robin cursor in lockstep: the cursor
                        // map is keyed on arbitrary caller-supplied strings and
                        // is lazily created by `workers_for`, so leaving stale
                        // entries behind leaks memory unboundedly on a
                        // never-dying server. When the last worker for a triple
                        // leaves, its cursor has no remaining meaning.
                        state.rotation.remove(&key);
                    }
                }
            }
        }

        Some(handle.namespaces)
    }

    fn state(&self) -> Result<MutexGuard<'_, RegistryState>, ServerError> {
        self.inner
            .lock()
            .map_err(|_| ServerError::lock_poisoned("connected worker registry"))
    }
}

/// Map a held [`WorkerDelivery`] to the wire [`WorkerTransport`] discriminant for
/// the WS3 `WorkerConnected` delta.
const fn transport_of(delivery: &WorkerDelivery) -> WorkerTransport {
    match delivery {
        WorkerDelivery::Grpc(_) => WorkerTransport::Grpc,
        #[cfg(feature = "liminal-transport")]
        WorkerDelivery::Liminal(_) => WorkerTransport::Liminal,
    }
}

/// Normalize a wire `node` string into an optional locality affinity: an empty
/// value (the proto3 default) carries no node, anything else is the worker's
/// advertised node id.
fn optional_node(node: &str) -> Option<String> {
    if node.is_empty() {
        None
    } else {
        Some(node.to_owned())
    }
}

/// Whether a worker satisfies an optional node filter. `None` (unpinned) matches
/// every worker; `Some(node)` matches only a worker advertising that exact node
/// (NODE affinity = require). A worker with no advertised node never matches a
/// pinned dispatch.
fn worker_matches_node(worker: &WorkerHandle, node: Option<&str>) -> bool {
    match node {
        None => true,
        Some(node) => worker.node() == Some(node),
    }
}

#[derive(Clone, Debug)]
struct WorkerRegistrationParts {
    worker_id: WorkerId,
    namespaces: BTreeSet<String>,
    task_queue: String,
    activity_types: BTreeSet<String>,
}

/// Registration token owned by the worker stream task.
///
/// Dropping the token performs best-effort cleanup for disconnect paths. Call
/// [`WorkerRegistration::deregister`] when the caller needs a typed poison error.
#[derive(Debug)]
pub struct WorkerRegistration {
    registry: ConnectedWorkerRegistry,
    parts: Option<WorkerRegistrationParts>,
}

impl WorkerRegistration {
    /// Worker id assigned to this registration.
    #[must_use]
    pub fn worker_id(&self) -> Option<WorkerId> {
        self.parts.as_ref().map(|parts| parts.worker_id)
    }

    /// Authorized namespace set for this registration.
    #[must_use]
    pub fn namespaces(&self) -> Option<&BTreeSet<String>> {
        self.parts.as_ref().map(|parts| &parts.namespaces)
    }

    /// Task queue (pool/flavour) this registration serves within each namespace.
    #[must_use]
    pub fn task_queue(&self) -> Option<&str> {
        self.parts.as_ref().map(|parts| parts.task_queue.as_str())
    }

    /// Activity types advertised by this registration.
    #[must_use]
    pub fn activity_types(&self) -> Option<&BTreeSet<String>> {
        self.parts.as_ref().map(|parts| &parts.activity_types)
    }

    /// Explicitly remove this worker from the registry.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn deregister(mut self) -> Result<(), ServerError> {
        let Some(parts) = self.parts.take() else {
            return Ok(());
        };
        self.registry.deregister(parts.worker_id)
    }
}

impl Drop for WorkerRegistration {
    fn drop(&mut self) {
        let Some(parts) = self.parts.take() else {
            return;
        };
        let removed_namespaces = self.registry.inner.lock().ok().and_then(|mut state| {
            ConnectedWorkerRegistry::remove_worker(&mut state, parts.worker_id)
        });
        if let Some(namespaces) = removed_namespaces {
            if let Some(metrics) = &self.registry.metrics {
                for namespace in &namespaces {
                    metrics.worker_disconnected(namespace);
                }
            }
            // A dropped registration token means the worker's stream/connection
            // went away — the truthful reason is Disconnect, not a fabricated
            // timeout/deregister distinction this path cannot prove.
            self.registry.emit_worker_disconnected(
                parts.worker_id,
                &namespaces,
                WorkerDeathReason::Disconnect,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::NamespaceMode;
    use crate::namespace::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};

    use super::*;

    fn guard() -> NamespaceGuard {
        NamespaceGuard::new(NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        ))
    }

    fn caller(namespace: &str) -> CallerIdentity {
        CallerIdentity::new("worker", [namespace.to_owned()])
    }

    fn registration(namespace: &str, activity_types: &[&str]) -> ProtoRegisterWorker {
        registration_with_queue(namespace, "", activity_types)
    }

    fn registration_with_queue(
        namespace: &str,
        task_queue: &str,
        activity_types: &[&str],
    ) -> ProtoRegisterWorker {
        registration_full(&[namespace], task_queue, "", activity_types)
    }

    fn registration_full(
        namespaces: &[&str],
        task_queue: &str,
        node: &str,
        activity_types: &[&str],
    ) -> ProtoRegisterWorker {
        ProtoRegisterWorker {
            namespaces: namespaces.iter().map(|value| (*value).to_owned()).collect(),
            activity_types: activity_types
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            task_queue: task_queue.to_owned(),
            node: node.to_owned(),
        }
    }

    fn multi_caller(namespaces: &[&str]) -> CallerIdentity {
        CallerIdentity::new("worker", namespaces.iter().map(|value| (*value).to_owned()))
    }

    #[tokio::test]
    async fn register_and_deregister_are_namespace_isolated() -> Result<(), ServerError> {
        let registry = ConnectedWorkerRegistry::default();
        let (tenant_a_tx, _tenant_a_rx) = mpsc::channel(1);
        let (tenant_b_tx, _tenant_b_rx) = mpsc::channel(1);

        let tenant_a = registry
            .accept_registration(
                &guard(),
                &caller("tenant-a"),
                &registration("tenant-a", &["charge", "charge"]),
                tenant_a_tx,
            )
            .await?;
        let tenant_b = registry
            .accept_registration(
                &guard(),
                &caller("tenant-b"),
                &registration("tenant-b", &["charge"]),
                tenant_b_tx,
            )
            .await?;

        let tq = DEFAULT_TASK_QUEUE;
        assert_eq!(
            registry.workers_for("tenant-a", tq, "charge", None)?.len(),
            1
        );
        assert_eq!(
            registry.workers_for("tenant-b", tq, "charge", None)?.len(),
            1
        );
        assert!(
            registry
                .workers_for("tenant-a", tq, "missing", None)?
                .is_empty()
        );

        let tenant_a_id = tenant_a.worker_id();
        tenant_a.deregister()?;

        assert!(
            registry
                .workers_for("tenant-a", tq, "charge", None)?
                .is_empty()
        );
        assert_eq!(
            registry.workers_for("tenant-b", tq, "charge", None)?.len(),
            1
        );
        assert_ne!(tenant_a_id, tenant_b.worker_id());

        tenant_b.deregister()?;
        assert!(
            registry
                .workers_for("tenant-b", tq, "charge", None)?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_namespace_is_not_registered() -> Result<(), ServerError> {
        let registry = ConnectedWorkerRegistry::default();
        let (tx, _rx) = mpsc::channel(1);
        let denied = registry
            .accept_registration(
                &guard(),
                &caller("tenant-a"),
                &registration("tenant-b", &["charge"]),
                tx,
            )
            .await;

        assert!(denied.is_err());
        assert!(
            registry
                .workers_for("tenant-b", DEFAULT_TASK_QUEUE, "charge", None)?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_queues_partition_disjoint_pools_within_one_namespace() -> Result<(), ServerError>
    {
        // Same namespace + same activity_type, two DIFFERENT task queues: the
        // pools are disjoint, a lookup for one queue never returns the other's
        // worker, and round-robin holds independently per (ns, tq, type) triple.
        let registry = ConnectedWorkerRegistry::default();
        let (norn_tx, _norn_rx) = mpsc::channel(1);
        let (claude_a_tx, _claude_a_rx) = mpsc::channel(1);
        let (claude_b_tx, _claude_b_rx) = mpsc::channel(1);

        let norn = registry
            .accept_registration(
                &guard(),
                &caller("local"),
                &registration_with_queue("local", "norn", &["dev"]),
                norn_tx,
            )
            .await?;
        // Two workers on the SAME (local, claude) pool to exercise round-robin.
        let claude_a = registry
            .accept_registration(
                &guard(),
                &caller("local"),
                &registration_with_queue("local", "claude", &["dev"]),
                claude_a_tx,
            )
            .await?;
        let claude_b = registry
            .accept_registration(
                &guard(),
                &caller("local"),
                &registration_with_queue("local", "claude", &["dev"]),
                claude_b_tx,
            )
            .await?;

        let norn_pool = registry.workers_for("local", "norn", "dev", None)?;
        assert_eq!(norn_pool.len(), 1, "norn pool has exactly its one worker");
        let norn_id = norn.worker_id().ok_or_else(missing_id)?;
        assert_eq!(norn_pool[0].id(), norn_id);

        let claude_pool = registry.workers_for("local", "claude", "dev", None)?;
        assert_eq!(
            claude_pool.len(),
            2,
            "claude pool sees only its two workers"
        );
        let claude_ids: BTreeSet<WorkerId> = claude_pool.iter().map(WorkerHandle::id).collect();
        assert!(
            !claude_ids.contains(&norn_id),
            "the norn worker must never appear in the claude pool"
        );

        // A dispatch targeting `norn` never reaches a `claude` worker, and vice
        // versa: the disjoint key is the boundary.
        assert!(
            !registry
                .workers_for("local", "norn", "dev", None)?
                .iter()
                .any(|worker| claude_ids.contains(&worker.id()))
        );

        // Round-robin per triple: the (local, claude, dev) cursor advances
        // independently and cycles through both claude workers, while the
        // (local, norn, dev) cursor keeps returning its single worker.
        let first = registry.workers_for("local", "claude", "dev", None)?[0].id();
        let second = registry.workers_for("local", "claude", "dev", None)?[0].id();
        assert_ne!(
            first, second,
            "claude pool round-robins across both workers"
        );
        assert_eq!(
            registry.workers_for("local", "norn", "dev", None)?[0].id(),
            norn_id,
            "the norn pool rotation is unaffected by claude traffic"
        );

        norn.deregister()?;
        claude_a.deregister()?;
        claude_b.deregister()?;
        Ok(())
    }

    #[tokio::test]
    async fn same_task_queue_in_different_namespaces_is_isolated() -> Result<(), ServerError> {
        // Same task_queue string, two DIFFERENT namespaces: namespace is the
        // correctness boundary, so the pools are isolated.
        let registry = ConnectedWorkerRegistry::default();
        let (local_tx, _local_rx) = mpsc::channel(1);
        let (remote_tx, _remote_rx) = mpsc::channel(1);

        let local = registry
            .accept_registration(
                &guard(),
                &caller("local"),
                &registration_with_queue("local", "gpu", &["render"]),
                local_tx,
            )
            .await?;
        let remote = registry
            .accept_registration(
                &guard(),
                &caller("remote"),
                &registration_with_queue("remote", "gpu", &["render"]),
                remote_tx,
            )
            .await?;

        let local_pool = registry.workers_for("local", "gpu", "render", None)?;
        let remote_pool = registry.workers_for("remote", "gpu", "render", None)?;
        assert_eq!(local_pool.len(), 1);
        assert_eq!(remote_pool.len(), 1);
        assert_ne!(
            local_pool[0].id(),
            remote_pool[0].id(),
            "a shared task_queue string does not merge two namespaces"
        );

        local.deregister()?;
        assert!(
            registry
                .workers_for("local", "gpu", "render", None)?
                .is_empty(),
            "deregistering the local worker leaves the remote namespace untouched"
        );
        assert_eq!(
            registry.workers_for("remote", "gpu", "render", None)?.len(),
            1
        );

        remote.deregister()?;
        Ok(())
    }

    #[tokio::test]
    async fn worker_serving_a_namespace_set_is_reachable_in_each() -> Result<(), ServerError> {
        // A worker advertising {a, b} is reachable for dispatch in BOTH a and b;
        // a worker in {a} is NOT reachable in b.
        let registry = ConnectedWorkerRegistry::default();
        let (ab_tx, _ab_rx) = mpsc::channel(1);
        let (a_tx, _a_rx) = mpsc::channel(1);

        let worker_ab = registry
            .accept_registration(
                &guard(),
                &multi_caller(&["a", "b"]),
                &registration_full(&["a", "b"], "default", "", &["dev"]),
                ab_tx,
            )
            .await?;
        let worker_a = registry
            .accept_registration(
                &guard(),
                &caller("a"),
                &registration_full(&["a"], "default", "", &["dev"]),
                a_tx,
            )
            .await?;

        let in_a = registry.workers_for("a", "default", "dev", None)?;
        let in_b = registry.workers_for("b", "default", "dev", None)?;
        let both_id = worker_ab.worker_id().ok_or_else(missing_id)?;
        let only_a_id = worker_a.worker_id().ok_or_else(missing_id)?;

        // Namespace a sees BOTH workers; namespace b sees ONLY the {a, b} worker.
        let a_ids: BTreeSet<WorkerId> = in_a.iter().map(WorkerHandle::id).collect();
        assert_eq!(a_ids, BTreeSet::from([both_id, only_a_id]));
        assert_eq!(in_b.len(), 1, "only the {{a, b}} worker is reachable in b");
        assert_eq!(in_b[0].id(), both_id);
        assert!(
            !in_b.iter().any(|worker| worker.id() == only_a_id),
            "the {{a}}-only worker must not be reachable in b"
        );

        // Deregistering the {a, b} worker removes it from BOTH buckets.
        worker_ab.deregister()?;
        assert!(
            registry
                .workers_for("b", "default", "dev", None)?
                .is_empty()
        );
        assert_eq!(registry.workers_for("a", "default", "dev", None)?.len(), 1);

        worker_a.deregister()?;
        Ok(())
    }

    #[tokio::test]
    async fn node_pin_filters_within_pool() -> Result<(), ServerError> {
        // Two workers in the same (namespace, task_queue) pool on different
        // nodes: unpinned round-robins across both; pinned to node N reaches
        // ONLY the worker(s) on N; pinned to a node with no worker finds none.
        let registry = ConnectedWorkerRegistry::default();
        let (n1_tx, _n1_rx) = mpsc::channel(1);
        let (n2_tx, _n2_rx) = mpsc::channel(1);

        let on_n1 = registry
            .accept_registration(
                &guard(),
                &caller("ns"),
                &registration_full(&["ns"], "tq", "n1", &["dev"]),
                n1_tx,
            )
            .await?;
        let on_n2 = registry
            .accept_registration(
                &guard(),
                &caller("ns"),
                &registration_full(&["ns"], "tq", "n2", &["dev"]),
                n2_tx,
            )
            .await?;
        let n1_id = on_n1.worker_id().ok_or_else(missing_id)?;
        let n2_id = on_n2.worker_id().ok_or_else(missing_id)?;

        // Unpinned: both workers are candidates and round-robin advances.
        let unpinned = registry.workers_for("ns", "tq", "dev", None)?;
        assert_eq!(unpinned.len(), 2, "unpinned reaches the whole pool");
        let first = registry.workers_for("ns", "tq", "dev", None)?[0].id();
        let second = registry.workers_for("ns", "tq", "dev", None)?[0].id();
        assert_ne!(first, second, "unpinned round-robins across both nodes");

        // Pinned to n1: only the n1 worker; pinned to n2: only the n2 worker.
        let pinned_n1 = registry.workers_for("ns", "tq", "dev", Some("n1"))?;
        assert_eq!(pinned_n1.len(), 1);
        assert_eq!(pinned_n1[0].id(), n1_id);
        let pinned_n2 = registry.workers_for("ns", "tq", "dev", Some("n2"))?;
        assert_eq!(pinned_n2.len(), 1);
        assert_eq!(pinned_n2[0].id(), n2_id);

        // select_worker honours the same filter.
        assert_eq!(
            registry
                .select_worker("ns", "tq", "dev", Some("n1"))?
                .map(|worker| worker.id()),
            Some(n1_id)
        );

        // Pinned to a node with no worker finds no candidate (the dispatcher
        // then waits via the same no-worker path the existing test exercises).
        assert!(
            registry
                .workers_for("ns", "tq", "dev", Some("absent"))?
                .is_empty(),
            "a pin to a node with no worker yields no candidate"
        );
        assert!(
            registry
                .select_worker("ns", "tq", "dev", Some("absent"))?
                .is_none()
        );

        on_n1.deregister()?;
        on_n2.deregister()?;
        Ok(())
    }

    #[tokio::test]
    async fn shared_node_id_round_robins_across_workers() -> Result<(), ServerError> {
        // Two workers SHARING a node id in the same pool: a dispatch pinned to
        // that node round-robins across BOTH (node is locality, not process).
        let registry = ConnectedWorkerRegistry::default();
        let (a_tx, _a_rx) = mpsc::channel(1);
        let (b_tx, _b_rx) = mpsc::channel(1);

        let worker_a = registry
            .accept_registration(
                &guard(),
                &caller("ns"),
                &registration_full(&["ns"], "tq", "shared", &["dev"]),
                a_tx,
            )
            .await?;
        let worker_b = registry
            .accept_registration(
                &guard(),
                &caller("ns"),
                &registration_full(&["ns"], "tq", "shared", &["dev"]),
                b_tx,
            )
            .await?;
        let a_id = worker_a.worker_id().ok_or_else(missing_id)?;
        let b_id = worker_b.worker_id().ok_or_else(missing_id)?;

        let pinned = registry.workers_for("ns", "tq", "dev", Some("shared"))?;
        assert_eq!(
            pinned.len(),
            2,
            "both workers on the shared node are candidates"
        );
        let pinned_ids: BTreeSet<WorkerId> = pinned.iter().map(WorkerHandle::id).collect();
        assert_eq!(pinned_ids, BTreeSet::from([a_id, b_id]));

        let first = registry.workers_for("ns", "tq", "dev", Some("shared"))?[0].id();
        let second = registry.workers_for("ns", "tq", "dev", Some("shared"))?[0].id();
        assert_ne!(
            first, second,
            "a pin to a shared node round-robins across both workers on it"
        );

        worker_a.deregister()?;
        worker_b.deregister()?;
        Ok(())
    }

    #[tokio::test]
    async fn rotation_cursor_is_pruned_when_last_worker_leaves() -> Result<(), ServerError> {
        // The round-robin cursor is keyed on arbitrary caller-supplied strings;
        // it must not outlive the pool it rotates, or a never-dying server leaks
        // memory. After the last worker for a triple deregisters, no cursor for
        // that triple may remain.
        let registry = ConnectedWorkerRegistry::default();
        let (tx, _rx) = mpsc::channel(1);
        let worker = registry
            .accept_registration(
                &guard(),
                &caller("ns"),
                &registration_full(&["ns"], "tq", "", &["dev"]),
                tx,
            )
            .await?;

        // Drive the lazy cursor insert.
        let _ = registry.workers_for("ns", "tq", "dev", None)?;
        let key = ActivityKey::new(PoolAddress::new("ns", "tq"), "dev");
        assert!(
            registry.state()?.rotation.contains_key(&key),
            "a lookup must have created the rotation cursor"
        );

        worker.deregister()?;
        let state = registry.state()?;
        assert!(
            !state.rotation.contains_key(&key),
            "the rotation cursor must be pruned once the last worker leaves"
        );
        assert!(
            !state.by_activity.contains_key(&key),
            "the activity bucket must also be gone"
        );
        Ok(())
    }

    fn missing_id() -> ServerError {
        ServerError::lock_poisoned("registration unexpectedly missing a worker id")
    }
}
