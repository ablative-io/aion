//! Runtime-owned, non-consuming fan-out records for beamr process exits.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use beamr::ets::OwnedTerm;
use beamr::process::ExitReason;
use beamr::scheduler::{ExitEvent, Scheduler};
use dashmap::DashMap;

use crate::{EngineError, Pid};

#[path = "process_exit_callback.rs"]
mod callback;
#[path = "process_exit_drainer.rs"]
mod drainer;
#[path = "process_exit_record.rs"]
mod record;

pub(super) use record::{
    ObservedProcessExit, OwnedProcessExitOutcome, ProcessExitCallback,
    ProcessExitObservationFailure, ProcessExitRecord,
};

struct RegistryLifecycle {
    closed: bool,
    /// Local children created by wrapped BEAM spawn BIFs and awaiting outcome release.
    unobserved_children: HashSet<Pid>,
}

/// Exclusive gate held from immediately before a scheduler spawn through pid classification.
pub(super) struct ProcessSpawnReservation<'a> {
    registry: &'a ProcessExitRegistry,
    lifecycle: MutexGuard<'a, RegistryLifecycle>,
}

impl ProcessSpawnReservation<'_> {
    pub(super) fn register(mut self, pid: Pid) -> Result<(), EngineError> {
        self.registry.register_locked(pid, &mut self.lifecycle)
    }

    pub(super) fn track_unobserved(mut self, pid: Pid) {
        self.lifecycle.unobserved_children.insert(pid);
    }
}

struct ExitDrainer {
    handle: Option<JoinHandle<Result<(), EngineError>>>,
    stopped: Receiver<()>,
}

/// Index of active owned exit records and owner of the one beamr event drainer.
pub(super) struct ProcessExitRegistry {
    records: DashMap<Pid, Arc<ProcessExitRecord>>,
    registered_through: AtomicU64,
    has_registered: AtomicBool,
    lifecycle: Mutex<RegistryLifecycle>,
    stop_drainer: AtomicBool,
    drainer: Mutex<ExitDrainer>,
    callbacks: callback::ProcessExitCallbackDispatcher,
    park_bound: Duration,
    shutdown_timeout: Duration,
    #[cfg(test)]
    pause_next_registration: AtomicBool,
    #[cfg(test)]
    registration_reached: AtomicU64,
    #[cfg(test)]
    registration_released: AtomicBool,
    #[cfg(test)]
    pause_next_publication: AtomicBool,
    #[cfg(test)]
    pause_drainer: AtomicBool,
    #[cfg(test)]
    drainer_paused: AtomicBool,
    #[cfg(test)]
    lag_recoveries: AtomicU64,
}

impl ProcessExitRegistry {
    pub(super) fn new(
        scheduler: Arc<Scheduler>,
        shutdown_timeout: Duration,
    ) -> Result<Arc<Self>, EngineError> {
        let subscription = scheduler
            .subscribe_exit_events()
            .ok_or(EngineError::ProcessExitSubscriptionUnavailable)?;
        let callbacks = callback::ProcessExitCallbackDispatcher::new(shutdown_timeout)?;
        let (stopped_sender, stopped) = mpsc::sync_channel(1);
        let registry = Arc::new(Self {
            records: DashMap::new(),
            registered_through: AtomicU64::new(0),
            has_registered: AtomicBool::new(false),
            lifecycle: Mutex::new(RegistryLifecycle {
                closed: false,
                unobserved_children: HashSet::new(),
            }),
            stop_drainer: AtomicBool::new(false),
            drainer: Mutex::new(ExitDrainer {
                handle: None,
                stopped,
            }),
            callbacks,
            park_bound: shutdown_timeout / 2,
            shutdown_timeout,
            #[cfg(test)]
            pause_next_registration: AtomicBool::new(false),
            #[cfg(test)]
            registration_reached: AtomicU64::new(0),
            #[cfg(test)]
            registration_released: AtomicBool::new(false),
            #[cfg(test)]
            pause_next_publication: AtomicBool::new(false),
            #[cfg(test)]
            pause_drainer: AtomicBool::new(false),
            #[cfg(test)]
            drainer_paused: AtomicBool::new(false),
            #[cfg(test)]
            lag_recoveries: AtomicU64::new(0),
        });
        let weak_registry = Arc::downgrade(&registry);
        let handle = std::thread::Builder::new()
            .name(String::from("aion-process-exit-drainer"))
            .spawn(move || {
                let result = drainer::run(&weak_registry, &scheduler, &subscription);
                let _ = stopped_sender.send(());
                result
            })
            .map_err(|error| EngineError::ProcessExitDrainerSpawn {
                reason: error.to_string(),
            })?;
        registry.lock_drainer()?.handle = Some(handle);
        Ok(registry)
    }

    pub(super) fn reserve_spawn(&self) -> Result<ProcessSpawnReservation<'_>, EngineError> {
        let lifecycle = self.lock_lifecycle()?;
        if lifecycle.closed {
            return Err(EngineError::ShuttingDown);
        }
        Ok(ProcessSpawnReservation {
            registry: self,
            lifecycle,
        })
    }

    fn register_locked(
        &self,
        pid: Pid,
        lifecycle: &mut RegistryLifecycle,
    ) -> Result<(), EngineError> {
        lifecycle.unobserved_children.remove(&pid);
        #[cfg(test)]
        self.pause_registration_if_requested(pid);
        #[cfg(test)]
        let pause_publication = self.pause_next_publication.swap(false, Ordering::AcqRel);
        let record = Arc::new(ProcessExitRecord::new(
            pid,
            #[cfg(test)]
            pause_publication,
        ));
        match self.records.entry(pid) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(EngineError::Runtime {
                    reason: format!("process {pid} already has a runtime-owned exit record"),
                });
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(record);
            }
        }
        self.registered_through.fetch_max(pid, Ordering::AcqRel);
        self.has_registered.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn get(&self, pid: Pid) -> Result<Arc<ProcessExitRecord>, EngineError> {
        self.find(pid).ok_or_else(|| {
            if self.is_retired(pid) {
                EngineError::ProcessExitAlreadyTerminal { process_id: pid }
            } else {
                EngineError::Runtime {
                    reason: format!("process {pid} has no runtime-owned exit outcome record"),
                }
            }
        })
    }

    pub(super) fn contains(&self, pid: Pid) -> bool {
        self.records.contains_key(&pid)
    }

    pub(super) fn find(&self, pid: Pid) -> Option<Arc<ProcessExitRecord>> {
        self.records
            .get(&pid)
            .map(|record| Arc::clone(record.value()))
    }

    pub(super) fn is_retired(&self, pid: Pid) -> bool {
        self.has_registered.load(Ordering::Acquire)
            && pid <= self.registered_through.load(Ordering::Acquire)
            && !self.contains(pid)
    }

    pub(super) fn has_terminal(&self, pid: Pid) -> Result<bool, EngineError> {
        if let Some(record) = self.find(pid) {
            record.is_terminal()
        } else {
            Ok(self.is_retired(pid))
        }
    }

    pub(super) fn is_current(&self, pid: Pid, expected: &Arc<ProcessExitRecord>) -> bool {
        self.records
            .get(&pid)
            .is_some_and(|current| Arc::ptr_eq(current.value(), expected))
    }

    pub(super) fn retire(&self, pid: Pid, expected: &Arc<ProcessExitRecord>) {
        if let dashmap::mapref::entry::Entry::Occupied(entry) = self.records.entry(pid) {
            if Arc::ptr_eq(entry.get(), expected) {
                entry.remove();
            }
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.records.len()
    }

    pub(super) fn begin_shutdown(&self) -> Result<Vec<Pid>, EngineError> {
        let mut lifecycle = self.lock_lifecycle()?;
        lifecycle.closed = true;
        let records: Vec<_> = self
            .records
            .iter()
            .map(|record| Arc::clone(record.value()))
            .collect();
        for record in &records {
            record.close_without_monitor()?;
        }
        Ok(records.iter().map(|record| record.pid).collect())
    }

    pub(super) fn close_and_join_all(&self) -> Result<(), EngineError> {
        let _ = self.begin_shutdown()?;
        self.stop_drainer.store(true, Ordering::Release);
        {
            let mut drainer = self.lock_drainer()?;
            if drainer.handle.is_some() {
                match drainer.stopped.recv_timeout(self.shutdown_timeout) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
                    Err(RecvTimeoutError::Timeout) => {
                        return Err(EngineError::ProcessExitDrainerShutdownTimedOut {
                            timeout_millis: self.shutdown_timeout.as_millis(),
                        });
                    }
                }
                let handle = drainer
                    .handle
                    .take()
                    .ok_or(EngineError::ProcessExitDrainerPanicked)?;
                handle
                    .join()
                    .map_err(|_| EngineError::ProcessExitDrainerPanicked)??;
            }
        }
        self.callbacks.shutdown()
    }

    pub(super) fn dispatch_callback(
        &self,
        record: &Arc<ProcessExitRecord>,
        callback: ProcessExitCallback,
        terminal: OwnedProcessExitOutcome,
    ) -> Result<(), EngineError> {
        match self.callbacks.dispatch(callback, terminal) {
            Ok(()) => Ok(()),
            Err(failure) => {
                let callback::CallbackDispatchFailure { callback, error } = *failure;
                record.restore_callback(callback)?;
                Err(error)
            }
        }
    }

    fn publish_record(
        &self,
        record: &Arc<ProcessExitRecord>,
        outcome: OwnedProcessExitOutcome,
    ) -> Result<(), EngineError> {
        if let Some(callback) = record.publish(outcome.clone())? {
            self.dispatch_callback(record, callback, outcome)?;
        }
        Ok(())
    }

    fn process_event(&self, scheduler: &Scheduler, event: ExitEvent) -> Result<(), EngineError> {
        match event {
            ExitEvent::Exited { pid, .. } => match scheduler.take_exit_outcome(pid) {
                Some((reason, result)) => self.publish_taken(scheduler, pid, reason, result),
                None if self.has_terminal(pid)? || !self.is_known(pid)? => Ok(()),
                None => Err(EngineError::ProcessExitOutcomeMissingAfterEvent { process_id: pid }),
            },
            ExitEvent::Lagged => {
                #[cfg(test)]
                self.lag_recoveries.fetch_add(1, Ordering::AcqRel);
                tracing::warn!("process exit event subscriber lagged; resynchronizing outcomes");
                self.resynchronize(scheduler)
            }
        }
    }

    /// Recover every pid classified before the reset while excluding concurrent spawns.
    ///
    /// Wrapped local BEAM spawn BIFs classify children under the same reservation gate,
    /// so lag recovery can take and discard their outcomes. Truly foreign scheduler pids
    /// are consumed on ordinary events but cannot be recovered after an overflow because
    /// beamr exposes no outcome-key enumeration API.
    fn resynchronize(&self, scheduler: &Scheduler) -> Result<(), EngineError> {
        let mut lifecycle = self.lock_lifecycle()?;
        let records: Vec<_> = self
            .records
            .iter()
            .map(|record| Arc::clone(record.value()))
            .collect();
        for record in records {
            if let Some((reason, result)) = scheduler.take_exit_outcome(record.pid) {
                self.publish_record(
                    &record,
                    Self::owned_outcome(scheduler, record.pid, reason, result),
                )?;
            }
        }
        let children: Vec<_> = lifecycle.unobserved_children.iter().copied().collect();
        for pid in children {
            if scheduler.take_exit_outcome(pid).is_some() {
                Self::discard_diagnostics(scheduler, pid);
                lifecycle.unobserved_children.remove(&pid);
            }
        }
        Ok(())
    }

    fn publish_taken(
        &self,
        scheduler: &Scheduler,
        pid: Pid,
        reason: ExitReason,
        result: OwnedTerm,
    ) -> Result<(), EngineError> {
        let mut lifecycle = self.lock_lifecycle()?;
        if let Some(record) = self.find(pid) {
            let outcome = Self::owned_outcome(scheduler, pid, reason, result);
            drop(lifecycle);
            self.publish_record(&record, outcome)
        } else {
            lifecycle.unobserved_children.remove(&pid);
            Self::discard_diagnostics(scheduler, pid);
            drop(result);
            Ok(())
        }
    }

    fn owned_outcome(
        scheduler: &Scheduler,
        pid: Pid,
        reason: ExitReason,
        result: OwnedTerm,
    ) -> OwnedProcessExitOutcome {
        OwnedProcessExitOutcome::Observed(Arc::new(ObservedProcessExit {
            reason,
            result,
            execution_error: scheduler.take_exit_error(pid),
            exception: scheduler.take_exit_exception(pid),
        }))
    }

    fn discard_diagnostics(scheduler: &Scheduler, pid: Pid) {
        drop(scheduler.take_exit_error(pid));
        drop(scheduler.take_exit_exception(pid));
    }

    fn is_known(&self, pid: Pid) -> Result<bool, EngineError> {
        Ok(self.contains(pid) || self.lock_lifecycle()?.unobserved_children.contains(&pid))
    }

    fn fail_unobserved(&self, failure: ProcessExitObservationFailure) {
        let records: Vec<_> = self
            .records
            .iter()
            .map(|record| Arc::clone(record.value()))
            .collect();
        for record in records {
            let outcome = OwnedProcessExitOutcome::ObservationFailed {
                process_id: record.pid,
                failure,
            };
            if let Err(error) = self.publish_record(&record, outcome) {
                tracing::error!(pid = record.pid, %error, "failed to publish exit observation failure");
            }
        }
    }

    fn all_records_terminal(&self) -> Result<bool, EngineError> {
        for record in &self.records {
            if !record.is_terminal()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn lock_lifecycle(&self) -> Result<MutexGuard<'_, RegistryLifecycle>, EngineError> {
        self.lifecycle
            .lock()
            .map_err(|_| EngineError::ProcessExitRegistryPoisoned)
    }

    fn lock_drainer(&self) -> Result<MutexGuard<'_, ExitDrainer>, EngineError> {
        self.drainer
            .lock()
            .map_err(|_| EngineError::ProcessExitDrainerPoisoned)
    }

    #[cfg(test)]
    fn pause_registration_if_requested(&self, pid: Pid) {
        if !self.pause_next_registration.swap(false, Ordering::AcqRel) {
            return;
        }
        self.registration_reached.store(pid, Ordering::Release);
        while !self.registration_released.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }

    #[cfg(test)]
    fn pause_if_requested(&self) {
        if !self.pause_drainer.load(Ordering::Acquire) {
            return;
        }
        self.drainer_paused.store(true, Ordering::Release);
        while self.pause_drainer.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
#[path = "process_exit_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "process_exit_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "process_exit_round12_tests.rs"]
mod round12_tests;
