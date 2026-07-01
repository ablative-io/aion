//! Active-execution registry keyed by workflow and run identifiers.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use aion_core::{Event, RunId, WorkflowId, status_from_events};

use crate::EngineError;

use super::handle::{Residency, WorkflowHandle};

type RegistryKey = (WorkflowId, RunId);
type HandleMap = HashMap<RegistryKey, WorkflowHandle>;

/// Secondary index mapping a workflow to its single live run.
///
/// Values are `(RunId, pid)`, not a bare pid: the [`RunId`] lets [`remove`]
/// compare-and-delete so a stale run's removal never evicts a newer run that
/// already overwrote the entry during the continue-as-new window.
///
/// [`remove`]: Registry::remove
type LivePidIndex = HashMap<WorkflowId, (RunId, u64)>;

/// Concurrency-safe registry of live workflow process handles.
///
/// The `index` is a `WorkflowId -> (RunId, pid)` secondary index maintained
/// alongside `handles` so the unmatched outbox-completion path can resolve a
/// workflow id to its live pid in O(1) without scanning the handle map.
///
/// # Lock ordering
///
/// `handles` is always locked before `index`; never the reverse. The
/// read-only [`Registry::live_pid`] lookup locks `index` alone. This fixed
/// order rules out lock-order inversion between the two mutexes.
#[derive(Debug, Default)]
pub struct Registry {
    handles: Mutex<HandleMap>,
    index: Mutex<LivePidIndex>,
}

impl Registry {
    /// Inserts or replaces the handle for a workflow run.
    ///
    /// Returns the previously registered handle for the same workflow/run, if any.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
    pub fn insert(
        &self,
        key: (WorkflowId, RunId),
        handle: WorkflowHandle,
    ) -> Result<Option<WorkflowHandle>, EngineError> {
        // Lock ordering: handles first, then index. Never the reverse.
        let mut handles = self.handles()?;
        let pid = handle.pid();
        let previous = handles.insert(key.clone(), handle);
        // Upsert the live-pid index: the newest run for a workflow id wins,
        // so a continue-as-new replacement points at the new run immediately.
        self.index()?.insert(key.0, (key.1, pid));
        Ok(previous)
    }

    /// Atomically inserts `handle` for `key` only if no handle is already
    /// registered for that exact `(workflow, run)`.
    ///
    /// Returns the handle now registered for `key`: the freshly inserted one when
    /// the slot was empty, or the pre-existing one when it was occupied (leaving
    /// that occupant untouched). The whole check-and-insert runs under the single
    /// `handles` lock, so two concurrent reopens of the same terminal workflow
    /// cannot both insert — exactly one wins the slot and the other observes the
    /// winner's handle. This is the per-workflow serialization point the reopen
    /// operation relies on (invariant #3): the winner's recorder is the sole
    /// writer.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
    pub fn insert_if_absent(
        &self,
        key: (WorkflowId, RunId),
        handle: WorkflowHandle,
    ) -> Result<WorkflowHandle, EngineError> {
        // Lock ordering: handles first, then index. Never the reverse.
        let mut handles = self.handles()?;
        if let Some(existing) = handles.get(&key) {
            return Ok(existing.clone());
        }
        let pid = handle.pid();
        handles.insert(key.clone(), handle.clone());
        self.index()?.insert(key.0, (key.1, pid));
        Ok(handle)
    }

    /// Looks up a live workflow run handle.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
    pub fn get(&self, id: &WorkflowId, run: &RunId) -> Result<Option<WorkflowHandle>, EngineError> {
        let handles = self.handles()?;
        Ok(handles.get(&(id.clone(), run.clone())).cloned())
    }

    /// Removes a live workflow run handle.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
    pub fn remove(
        &self,
        id: &WorkflowId,
        run: &RunId,
    ) -> Result<Option<WorkflowHandle>, EngineError> {
        // Lock ordering: handles first, then index. Never the reverse.
        let mut handles = self.handles()?;
        let removed = handles.remove(&(id.clone(), run.clone()));
        // Compare-and-delete: only drop the index entry when it still points
        // at the run being removed. During continue-as-new a newer run already
        // overwrote the entry, and that newer run must survive this removal.
        let mut index = self.index()?;
        if let std::collections::hash_map::Entry::Occupied(entry) = index.entry(id.clone()) {
            if entry.get().0 == *run {
                entry.remove();
            }
        }
        Ok(removed)
    }

    /// Returns a snapshot of all live handles without holding the registry lock.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
    pub fn list(&self) -> Result<Vec<WorkflowHandle>, EngineError> {
        let handles = self.handles()?;
        Ok(handles.values().cloned().collect())
    }

    /// Updates only the engine-internal residency for a live workflow run.
    ///
    /// The projected workflow status is not read or changed. If the workflow run
    /// is not registered, no cache is updated and `Ok(None)` is returned.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
    pub fn replace_residency(
        &self,
        id: &WorkflowId,
        run: &RunId,
        residency: Residency,
    ) -> Result<Option<WorkflowHandle>, EngineError> {
        let mut handles = self.handles()?;
        let Some(handle) = handles.get_mut(&(id.clone(), run.clone())) else {
            return Ok(None);
        };

        handle.replace_residency(residency);
        Ok(Some(handle.clone()))
    }

    /// Reconciles a cached handle status against the core event projection.
    ///
    /// The projected status always wins. If the workflow run is not registered,
    /// no cache is updated and `Ok(None)` is returned. Residency is not read or changed.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
    pub fn reconcile(
        &self,
        id: &WorkflowId,
        run: &RunId,
        events: &[Event],
    ) -> Result<Option<WorkflowHandle>, EngineError> {
        let projected = status_from_events(events);
        let mut handles = self.handles()?;
        let Some(handle) = handles.get_mut(&(id.clone(), run.clone())) else {
            return Ok(None);
        };

        handle.replace_projected_status(projected);
        Ok(Some(handle.clone()))
    }

    /// Resolves a workflow id to the pid of its single live run, if any.
    ///
    /// Reads only the secondary index, so it never contends the handle map.
    /// Returns `Ok(None)` when no run for the workflow is currently live — the
    /// expected stale-completion case after a crash or eviction, before
    /// recovery re-arms the run.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the index lock was poisoned.
    pub fn live_pid(&self, workflow_id: &WorkflowId) -> Result<Option<u64>, EngineError> {
        let index = self.index()?;
        Ok(index.get(workflow_id).map(|(_, pid)| *pid))
    }

    /// Resolves a workflow id to its single live run and that run's pid.
    ///
    /// Run-aware twin of [`Self::live_pid`]: the returned [`RunId`] lets the
    /// caller reject a completion that belongs to a superseded run (e.g. a
    /// prior run after continue-as-new) so the new run's reused ordinal space
    /// is never resolved by a dead run's late activity completion (OBX-011).
    ///
    /// Reads only the secondary index, so it never contends the handle map.
    /// Returns `Ok(None)` when no run for the workflow is currently live.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the index lock was poisoned.
    pub fn live_run_pid(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<(RunId, u64)>, EngineError> {
        let index = self.index()?;
        Ok(index.get(workflow_id).cloned())
    }

    fn handles(&self) -> Result<MutexGuard<'_, HandleMap>, EngineError> {
        self.handles
            .lock()
            .map_err(|_| EngineError::RegistryPoisoned)
    }

    fn index(&self) -> Result<MutexGuard<'_, LivePidIndex>, EngineError> {
        self.index.lock().map_err(|_| EngineError::RegistryPoisoned)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, EventEnvelope, Payload, PayloadError, WorkflowId, WorkflowStatus};
    use aion_package::ContentHash;
    use chrono::Utc;
    use serde_json::json;

    use crate::EngineError;
    use crate::registry::handle::{
        CompletionNotifier, HandleResidency, WorkflowHandle, WorkflowHandleParts,
    };

    use super::Registry;

    type TestResult = Result<(), TestError>;

    #[derive(Debug)]
    enum TestError {
        Engine(EngineError),
        Payload(PayloadError),
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Engine(error) => write!(formatter, "{error}"),
                Self::Payload(error) => write!(formatter, "{error}"),
            }
        }
    }

    impl std::error::Error for TestError {}

    impl From<EngineError> for TestError {
        fn from(error: EngineError) -> Self {
            Self::Engine(error)
        }
    }

    impl From<PayloadError> for TestError {
        fn from(error: PayloadError) -> Self {
            Self::Payload(error)
        }
    }

    fn assert_send_sync<T: Send + Sync>() {}

    fn hash(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    fn handle(pid: u64, version_byte: u8, status: WorkflowStatus) -> WorkflowHandle {
        let workflow_id = WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let store = Arc::new(aion_store::InMemoryStore::default());
        let recorder = crate::durability::Recorder::new(workflow_id.clone(), store);
        WorkflowHandle::new(WorkflowHandleParts {
            workflow_id,
            run_id,
            pid,
            workflow_type: "checkout".to_owned(),
            namespace: String::from("default"),
            loaded_version: hash(version_byte),
            cached_status: status,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        })
    }

    fn envelope(workflow_id: &aion_core::WorkflowId, seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: workflow_id.clone(),
        }
    }

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn started(workflow_id: &aion_core::WorkflowId) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(workflow_id, 1),
            workflow_type: String::from("checkout"),
            input: payload("input")?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn completed(workflow_id: &aion_core::WorkflowId) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowCompleted {
            envelope: envelope(workflow_id, 2),
            result: payload("result")?,
        })
    }

    fn cancelled(workflow_id: &aion_core::WorkflowId) -> Event {
        Event::WorkflowCancelled {
            envelope: envelope(workflow_id, 2),
            reason: String::from("caller requested cancellation"),
        }
    }

    #[test]
    fn registry_is_send_sync() {
        assert_send_sync::<Registry>();
    }

    #[test]
    fn stores_two_runs_for_the_same_workflow_without_shadowing() -> Result<(), EngineError> {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let first_run = aion_core::RunId::new_v4();
        let second_run = aion_core::RunId::new_v4();
        let first = handle(1, 1, WorkflowStatus::Running);
        let second = handle(2, 2, WorkflowStatus::Completed);

        assert!(
            registry
                .insert((workflow_id.clone(), first_run.clone()), first.clone())?
                .is_none()
        );
        assert!(
            registry
                .insert((workflow_id.clone(), second_run.clone()), second.clone())?
                .is_none()
        );

        assert_eq!(registry.get(&workflow_id, &first_run)?, Some(first));
        assert_eq!(registry.get(&workflow_id, &second_run)?, Some(second));

        let stale_run = aion_core::RunId::new_v4();
        assert_eq!(registry.get(&workflow_id, &stale_run)?, None);
        Ok(())
    }

    #[test]
    fn remove_deletes_only_the_requested_run() -> Result<(), EngineError> {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let first_run = aion_core::RunId::new_v4();
        let second_run = aion_core::RunId::new_v4();
        let first = handle(1, 1, WorkflowStatus::Running);
        let second = handle(2, 2, WorkflowStatus::Running);

        registry.insert((workflow_id.clone(), first_run.clone()), first.clone())?;
        registry.insert((workflow_id.clone(), second_run.clone()), second.clone())?;

        assert_eq!(registry.remove(&workflow_id, &first_run)?, Some(first));
        assert_eq!(registry.get(&workflow_id, &first_run)?, None);
        assert_eq!(registry.get(&workflow_id, &second_run)?, Some(second));
        Ok(())
    }

    #[test]
    fn live_pid_tracks_newest_run_across_continue_as_new() -> Result<(), EngineError> {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let first_run = aion_core::RunId::new_v4();
        let second_run = aion_core::RunId::new_v4();

        // An unknown workflow has no live pid.
        assert_eq!(registry.live_pid(&workflow_id)?, None);

        // Insert resolves the workflow to its run's pid.
        registry.insert(
            (workflow_id.clone(), first_run.clone()),
            handle(1, 1, WorkflowStatus::Running),
        )?;
        assert_eq!(registry.live_pid(&workflow_id)?, Some(1));

        // Continue-as-new inserts the new run, then removes the old one. The
        // index must track the newest run, and the stale removal must not
        // evict it (compare-and-delete on RunId).
        registry.insert(
            (workflow_id.clone(), second_run.clone()),
            handle(2, 2, WorkflowStatus::Running),
        )?;
        assert_eq!(registry.live_pid(&workflow_id)?, Some(2));
        registry.remove(&workflow_id, &first_run)?;
        assert_eq!(registry.live_pid(&workflow_id)?, Some(2));

        // Removing the live run clears the index entry.
        registry.remove(&workflow_id, &second_run)?;
        assert_eq!(registry.live_pid(&workflow_id)?, None);
        Ok(())
    }

    #[test]
    fn insert_if_absent_wins_the_slot_and_rejects_the_racer() -> Result<(), EngineError> {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run = aion_core::RunId::new_v4();
        let first = handle(1, 1, WorkflowStatus::Running);
        let second = handle(2, 2, WorkflowStatus::Running);

        // The empty slot accepts the first handle.
        let registered =
            registry.insert_if_absent((workflow_id.clone(), run.clone()), first.clone())?;
        assert_eq!(registered.pid(), 1);
        assert_eq!(registry.live_pid(&workflow_id)?, Some(1));

        // A second insert_if_absent observes the existing occupant untouched.
        let observed =
            registry.insert_if_absent((workflow_id.clone(), run.clone()), second)?;
        assert_eq!(
            observed.pid(),
            1,
            "the second insert must return the winner's handle, not overwrite it"
        );
        assert_eq!(registry.get(&workflow_id, &run)?, Some(first));
        Ok(())
    }

    #[test]
    fn list_returns_snapshot_handles() -> Result<(), EngineError> {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let first_run = aion_core::RunId::new_v4();
        let second_run = aion_core::RunId::new_v4();

        registry.insert(
            (workflow_id.clone(), first_run),
            handle(1, 1, WorkflowStatus::Running),
        )?;
        registry.insert(
            (workflow_id, second_run),
            handle(2, 2, WorkflowStatus::Running),
        )?;

        let mut pids = registry
            .list()?
            .into_iter()
            .map(|handle| handle.pid())
            .collect::<Vec<_>>();
        pids.sort_unstable();

        assert_eq!(pids, vec![1, 2]);
        Ok(())
    }

    #[test]
    fn poisoned_lock_returns_typed_registry_error() {
        let registry = Arc::new(Registry::default());
        let poisoner_registry = Arc::clone(&registry);
        let poisoner = std::thread::spawn(move || {
            let guard = poisoner_registry.handles.lock();
            assert!(guard.is_ok());
            std::panic::resume_unwind(Box::new("poison registry lock"));
        });

        assert!(poisoner.join().is_err());
        assert!(matches!(
            registry.list(),
            Err(EngineError::RegistryPoisoned)
        ));
    }

    #[test]
    fn reconcile_updates_completed_projection() -> TestResult {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        registry.insert(
            (workflow_id.clone(), run_id.clone()),
            handle(1, 1, WorkflowStatus::Running),
        )?;
        let events = vec![started(&workflow_id)?, completed(&workflow_id)?];

        let reconciled = registry.reconcile(&workflow_id, &run_id, &events)?;

        assert_eq!(
            reconciled.map(|handle| handle.cached_status()),
            Some(WorkflowStatus::Completed)
        );
        assert_eq!(
            registry
                .get(&workflow_id, &run_id)?
                .map(|handle| handle.cached_status()),
            Some(WorkflowStatus::Completed)
        );
        Ok(())
    }

    #[test]
    fn reconcile_updates_cancelled_projection() -> TestResult {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        registry.insert(
            (workflow_id.clone(), run_id.clone()),
            handle(1, 1, WorkflowStatus::Running),
        )?;
        let events = vec![started(&workflow_id)?, cancelled(&workflow_id)];

        let reconciled = registry.reconcile(&workflow_id, &run_id, &events)?;

        assert_eq!(
            reconciled.map(|handle| handle.cached_status()),
            Some(WorkflowStatus::Cancelled)
        );
        Ok(())
    }

    #[test]
    fn reconcile_projection_wins_over_disagreeing_cache() -> TestResult {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        registry.insert(
            (workflow_id.clone(), run_id.clone()),
            handle(1, 1, WorkflowStatus::Failed),
        )?;
        let events = vec![started(&workflow_id)?];

        let reconciled = registry.reconcile(&workflow_id, &run_id, &events)?;

        assert_eq!(
            reconciled.map(|handle| handle.cached_status()),
            Some(WorkflowStatus::Running)
        );
        Ok(())
    }

    #[test]
    fn reconcile_missing_handle_is_noop() -> TestResult {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let events = vec![started(&workflow_id)?];

        assert_eq!(registry.reconcile(&workflow_id, &run_id, &events)?, None);
        Ok(())
    }
}
