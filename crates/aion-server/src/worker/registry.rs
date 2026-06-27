//! Connected-worker registry keyed by worker-pool address and activity type.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use aion_proto::{ProtoActivityTask, ProtoRegisterWorker};
use tokio::sync::{Notify, mpsc};

use crate::error::ServerError;
use crate::namespace::{CallerIdentity, NamespaceGuard, NamespaceOperation};
use crate::observability::Metrics;

/// The literal task queue an empty/absent selector normalizes to.
///
/// A worker-pool address has two disjoint dimensions; the second one
/// (`task_queue`) is a liveness selector, not a correctness boundary. An empty
/// `task_queue` is normalized to this one named default pool so a producer that
/// names no queue and a worker that advertises none both land on the same pool.
pub const DEFAULT_TASK_QUEUE: &str = "default";

/// Server-side handle used to push activity tasks to a connected worker stream.
pub type WorkerTaskSender = mpsc::Sender<WorkerMessage>;

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
#[derive(Clone, Debug)]
pub struct WorkerHandle {
    id: WorkerId,
    pool: PoolAddress,
    activity_types: BTreeSet<String>,
    sender: WorkerTaskSender,
}

impl WorkerHandle {
    /// Worker identifier assigned by this server process.
    #[must_use]
    pub const fn id(&self) -> WorkerId {
        self.id
    }

    /// Worker-pool address (namespace + task queue) this worker serves.
    #[must_use]
    pub const fn pool(&self) -> &PoolAddress {
        &self.pool
    }

    /// Namespace authorized for this worker stream.
    #[must_use]
    pub fn namespace(&self) -> &str {
        self.pool.namespace()
    }

    /// Task queue (pool/flavour) this worker serves within its namespace.
    #[must_use]
    pub fn task_queue(&self) -> &str {
        self.pool.task_queue()
    }

    /// Activity types advertised by this worker.
    #[must_use]
    pub fn activity_types(&self) -> &BTreeSet<String> {
        &self.activity_types
    }

    /// Sender used by dispatch to push work to the stream task.
    #[must_use]
    pub fn sender(&self) -> &WorkerTaskSender {
        &self.sender
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
    worker_arrived: Arc<Notify>,
}

impl Default for ConnectedWorkerRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState::default())),
            metrics: None,
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
            worker_arrived: Arc::new(Notify::new()),
        }
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
        let scoped = guard
            .scope(caller, &NamespaceOperation::register_worker(registration))
            .await?;
        // The authorized namespace is the correctness boundary; the wire's
        // task_queue is the disjoint pool selector within it (empty normalizes
        // to the named default pool inside `PoolAddress::new`).
        let pool = PoolAddress::new(scoped.namespace(), registration.task_queue.clone());
        self.register_pool(pool, registration.activity_types.iter(), sender)
    }

    /// Insert an already-authorized worker stream into a pool addressed by
    /// `namespace` alone, using the named default task queue.
    ///
    /// Convenience over [`Self::register_pool`] for callers that do not select a
    /// task queue (notably tests of the default pool).
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
        self.register_pool(
            PoolAddress::new(namespace, DEFAULT_TASK_QUEUE),
            activity_types,
            sender,
        )
    }

    /// Insert an already-authorized worker stream into an explicit worker pool.
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
        let activity_types = activity_types.into_iter().cloned().collect::<BTreeSet<_>>();
        let mut state = self.state()?;
        let worker_id = WorkerId(state.next_worker_id);
        state.next_worker_id = state.next_worker_id.saturating_add(1);

        let handle = WorkerHandle {
            id: worker_id,
            pool: pool.clone(),
            activity_types: activity_types.clone(),
            sender,
        };

        for activity_type in &activity_types {
            state
                .by_activity
                .entry(ActivityKey::new(pool.clone(), activity_type.clone()))
                .or_default()
                .insert(worker_id, handle.clone());
        }
        state.workers.insert(worker_id, handle);
        drop(state);

        if let Some(metrics) = &self.metrics {
            metrics.worker_connected(pool.namespace());
        }

        self.worker_arrived.notify_waiters();

        Ok(WorkerRegistration {
            registry: self.clone(),
            parts: Some(WorkerRegistrationParts {
                worker_id,
                pool,
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
    ) -> Result<Vec<WorkerHandle>, ServerError> {
        let mut state = self.state()?;
        let key = ActivityKey::new(PoolAddress::new(namespace, task_queue), activity_type);
        let mut workers: Vec<WorkerHandle> = state
            .by_activity
            .get(&key)
            .map(|workers| workers.values().cloned().collect())
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
            if worker
                .sender()
                .try_send(WorkerMessage::DrainRequest)
                .is_ok()
            {
                delivered = delivered.saturating_add(1);
            } else {
                self.deregister(worker.id())?;
            }
        }
        Ok(delivered)
    }

    /// Select one worker for the `(namespace, task_queue, activity_type)` pool.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn select_worker(
        &self,
        namespace: &str,
        task_queue: &str,
        activity_type: &str,
    ) -> Result<Option<WorkerHandle>, ServerError> {
        let state = self.state()?;
        let key = ActivityKey::new(PoolAddress::new(namespace, task_queue), activity_type);
        Ok(state
            .by_activity
            .get(&key)
            .and_then(|workers| workers.values().min_by_key(|worker| worker.id).cloned()))
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
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn deregister(&self, worker_id: WorkerId) -> Result<(), ServerError> {
        let mut state = self.state()?;
        let removed_namespace = Self::remove_worker(&mut state, worker_id);
        drop(state);

        if let (Some(namespace), Some(metrics)) = (removed_namespace, &self.metrics) {
            metrics.worker_disconnected(&namespace);
        }

        Ok(())
    }

    fn remove_worker(state: &mut RegistryState, worker_id: WorkerId) -> Option<String> {
        let handle = state.workers.remove(&worker_id)?;
        let namespace = handle.pool.namespace().to_owned();

        for activity_type in handle.activity_types {
            let key = ActivityKey::new(handle.pool.clone(), activity_type);
            if let Some(workers) = state.by_activity.get_mut(&key) {
                workers.remove(&worker_id);
                if workers.is_empty() {
                    state.by_activity.remove(&key);
                }
            }
        }

        Some(namespace)
    }

    fn state(&self) -> Result<MutexGuard<'_, RegistryState>, ServerError> {
        self.inner
            .lock()
            .map_err(|_| ServerError::lock_poisoned("connected worker registry"))
    }
}

#[derive(Clone, Debug)]
struct WorkerRegistrationParts {
    worker_id: WorkerId,
    pool: PoolAddress,
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

    /// Authorized namespace for this registration.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.parts.as_ref().map(|parts| parts.pool.namespace())
    }

    /// Task queue (pool/flavour) this registration serves within its namespace.
    #[must_use]
    pub fn task_queue(&self) -> Option<&str> {
        self.parts.as_ref().map(|parts| parts.pool.task_queue())
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
        if let Ok(mut state) = self.registry.inner.lock() {
            let removed_namespace =
                ConnectedWorkerRegistry::remove_worker(&mut state, parts.worker_id);
            if let (Some(namespace), Some(metrics)) = (removed_namespace, &self.registry.metrics) {
                metrics.worker_disconnected(&namespace);
            }
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
        ProtoRegisterWorker {
            namespace: namespace.to_owned(),
            activity_types: activity_types
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            task_queue: task_queue.to_owned(),
        }
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
        assert_eq!(registry.workers_for("tenant-a", tq, "charge")?.len(), 1);
        assert_eq!(registry.workers_for("tenant-b", tq, "charge")?.len(), 1);
        assert!(registry.workers_for("tenant-a", tq, "missing")?.is_empty());

        let tenant_a_id = tenant_a.worker_id();
        tenant_a.deregister()?;

        assert!(registry.workers_for("tenant-a", tq, "charge")?.is_empty());
        assert_eq!(registry.workers_for("tenant-b", tq, "charge")?.len(), 1);
        assert_ne!(tenant_a_id, tenant_b.worker_id());

        tenant_b.deregister()?;
        assert!(registry.workers_for("tenant-b", tq, "charge")?.is_empty());
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
                .workers_for("tenant-b", DEFAULT_TASK_QUEUE, "charge")?
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

        let norn_pool = registry.workers_for("local", "norn", "dev")?;
        assert_eq!(norn_pool.len(), 1, "norn pool has exactly its one worker");
        let norn_id = norn.worker_id().ok_or_else(missing_id)?;
        assert_eq!(norn_pool[0].id(), norn_id);

        let claude_pool = registry.workers_for("local", "claude", "dev")?;
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
                .workers_for("local", "norn", "dev")?
                .iter()
                .any(|worker| claude_ids.contains(&worker.id()))
        );

        // Round-robin per triple: the (local, claude, dev) cursor advances
        // independently and cycles through both claude workers, while the
        // (local, norn, dev) cursor keeps returning its single worker.
        let first = registry.workers_for("local", "claude", "dev")?[0].id();
        let second = registry.workers_for("local", "claude", "dev")?[0].id();
        assert_ne!(
            first, second,
            "claude pool round-robins across both workers"
        );
        assert_eq!(
            registry.workers_for("local", "norn", "dev")?[0].id(),
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

        let local_pool = registry.workers_for("local", "gpu", "render")?;
        let remote_pool = registry.workers_for("remote", "gpu", "render")?;
        assert_eq!(local_pool.len(), 1);
        assert_eq!(remote_pool.len(), 1);
        assert_ne!(
            local_pool[0].id(),
            remote_pool[0].id(),
            "a shared task_queue string does not merge two namespaces"
        );

        local.deregister()?;
        assert!(
            registry.workers_for("local", "gpu", "render")?.is_empty(),
            "deregistering the local worker leaves the remote namespace untouched"
        );
        assert_eq!(registry.workers_for("remote", "gpu", "render")?.len(), 1);

        remote.deregister()?;
        Ok(())
    }

    fn missing_id() -> ServerError {
        ServerError::lock_poisoned("registration unexpectedly missing a worker id")
    }
}
