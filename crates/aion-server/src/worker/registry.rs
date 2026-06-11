//! Connected-worker registry keyed by namespace and activity type.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use aion_proto::{ProtoActivityTask, ProtoRegisterWorker};
use tokio::sync::mpsc;

use crate::error::ServerError;
use crate::namespace::{CallerIdentity, NamespaceGuard, NamespaceOperation};
use crate::observability::Metrics;

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

type ActivityKey = (String, String);
type WorkerMap = HashMap<WorkerId, WorkerHandle>;
type RegistryMap = HashMap<ActivityKey, WorkerMap>;

/// Stable identifier assigned to a connected worker stream.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkerId(u64);

/// Cloneable handle for a registered worker stream.
#[derive(Clone, Debug)]
pub struct WorkerHandle {
    id: WorkerId,
    namespace: String,
    activity_types: BTreeSet<String>,
    sender: WorkerTaskSender,
}

impl WorkerHandle {
    /// Worker identifier assigned by this server process.
    #[must_use]
    pub const fn id(&self) -> WorkerId {
        self.id
    }

    /// Namespace authorized for this worker stream.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
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
}

impl Default for RegistryState {
    fn default() -> Self {
        Self {
            next_worker_id: 1,
            workers: BTreeMap::new(),
            by_activity: HashMap::new(),
        }
    }
}

/// Cloneable registry of currently connected worker streams.
#[derive(Clone, Debug, Default)]
pub struct ConnectedWorkerRegistry {
    inner: Arc<Mutex<RegistryState>>,
    metrics: Option<Metrics>,
}

impl ConnectedWorkerRegistry {
    /// Build a registry that records connected-worker gauge updates.
    #[must_use]
    pub fn with_metrics(metrics: Metrics) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState::default())),
            metrics: Some(metrics),
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
        self.register(
            scoped.namespace(),
            registration.activity_types.iter(),
            sender,
        )
    }

    /// Insert an already-authorized worker stream.
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
        let namespace = namespace.into();
        let activity_types = activity_types.into_iter().cloned().collect::<BTreeSet<_>>();
        let mut state = self.state()?;
        let worker_id = WorkerId(state.next_worker_id);
        state.next_worker_id = state.next_worker_id.saturating_add(1);

        let handle = WorkerHandle {
            id: worker_id,
            namespace: namespace.clone(),
            activity_types: activity_types.clone(),
            sender,
        };

        for activity_type in &activity_types {
            state
                .by_activity
                .entry((namespace.clone(), activity_type.clone()))
                .or_default()
                .insert(worker_id, handle.clone());
        }
        state.workers.insert(worker_id, handle);
        drop(state);

        if let Some(metrics) = &self.metrics {
            metrics.worker_connected(&namespace);
        }

        Ok(WorkerRegistration {
            registry: self.clone(),
            parts: Some(WorkerRegistrationParts {
                worker_id,
                namespace,
                activity_types,
            }),
        })
    }

    /// Return a snapshot of workers registered for the namespace and activity type.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn workers_for(
        &self,
        namespace: &str,
        activity_type: &str,
    ) -> Result<Vec<WorkerHandle>, ServerError> {
        let state = self.state()?;
        let key = (namespace.to_owned(), activity_type.to_owned());
        Ok(state
            .by_activity
            .get(&key)
            .map(|workers| workers.values().cloned().collect())
            .unwrap_or_default())
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

    /// Select one worker for the namespace and activity type.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn select_worker(
        &self,
        namespace: &str,
        activity_type: &str,
    ) -> Result<Option<WorkerHandle>, ServerError> {
        let state = self.state()?;
        let key = (namespace.to_owned(), activity_type.to_owned());
        Ok(state
            .by_activity
            .get(&key)
            .and_then(|workers| workers.values().min_by_key(|worker| worker.id).cloned()))
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
        let namespace = handle.namespace.clone();

        for activity_type in handle.activity_types {
            let key = (handle.namespace.clone(), activity_type);
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
    namespace: String,
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
        self.parts.as_ref().map(|parts| parts.namespace.as_str())
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
    use crate::namespace::{NamespaceResolver, StaticWorkflowNamespaces};

    use super::*;

    fn guard() -> NamespaceGuard {
        NamespaceGuard::new(NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
        ))
    }

    fn caller(namespace: &str) -> CallerIdentity {
        CallerIdentity::new("worker", [namespace.to_owned()])
    }

    fn registration(namespace: &str, activity_types: &[&str]) -> ProtoRegisterWorker {
        ProtoRegisterWorker {
            namespace: namespace.to_owned(),
            activity_types: activity_types
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
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

        assert_eq!(registry.workers_for("tenant-a", "charge")?.len(), 1);
        assert_eq!(registry.workers_for("tenant-b", "charge")?.len(), 1);
        assert!(registry.workers_for("tenant-a", "missing")?.is_empty());

        let tenant_a_id = tenant_a.worker_id();
        tenant_a.deregister()?;

        assert!(registry.workers_for("tenant-a", "charge")?.is_empty());
        assert_eq!(registry.workers_for("tenant-b", "charge")?.len(), 1);
        assert_ne!(tenant_a_id, tenant_b.worker_id());

        tenant_b.deregister()?;
        assert!(registry.workers_for("tenant-b", "charge")?.is_empty());
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
        assert!(registry.workers_for("tenant-b", "charge")?.is_empty());
        Ok(())
    }
}
