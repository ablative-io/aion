//! Runtime-owned, non-consuming fan-out records for beamr process exits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use beamr::error::ExecError;
use beamr::ets::OwnedTerm;
use beamr::process::ExitReason;
use beamr::scheduler::{ExitEvent, OwnedException, Scheduler};
use dashmap::DashMap;

use crate::{EngineError, Pid};

use super::monitor::MonitorInstallation;

#[path = "process_exit_callback.rs"]
mod callback;
#[path = "process_exit_drainer.rs"]
mod drainer;

pub(super) type ProcessExitCallback = Box<dyn FnOnce(OwnedProcessExitOutcome) + Send + 'static>;

/// The single captured result of beamr's consuming exit-outcome API.
pub(super) struct ObservedProcessExit {
    pub(super) reason: ExitReason,
    pub(super) result: OwnedTerm,
    pub(super) execution_error: Option<ExecError>,
    pub(super) exception: Option<OwnedException>,
}

/// A defensive failure from the runtime's exclusive exit-event consumer.
#[derive(Clone, Copy)]
pub(super) enum ProcessExitObservationFailure {
    /// An `Exited` event had no takeable outcome and was not a stale post-resync event.
    OutcomeMissingAfterEvent,
    /// beamr disconnected its publisher while the runtime still owned the scheduler.
    EventStreamDisconnected,
}

impl ProcessExitObservationFailure {
    pub(super) fn into_engine_error(self, process_id: Pid) -> EngineError {
        match self {
            Self::OutcomeMissingAfterEvent => {
                EngineError::ProcessExitOutcomeMissingAfterEvent { process_id }
            }
            Self::EventStreamDisconnected => EngineError::ProcessExitEventStreamDisconnected,
        }
    }
}

/// Terminal state cached independently of beamr's legacy tombstone FIFO.
#[derive(Clone)]
pub(super) enum OwnedProcessExitOutcome {
    /// The durable beamr outcome was captured exactly once by the drainer.
    Observed(Arc<ObservedProcessExit>),
    /// Observation failed on a defensive path that the beamr event contract makes unreachable.
    ObservationFailed {
        process_id: Pid,
        failure: ProcessExitObservationFailure,
    },
}

struct ProcessExitState {
    terminal: Option<OwnedProcessExitOutcome>,
    callback: Option<ProcessExitCallback>,
    closed: bool,
}

/// One `pid` generation's cached exit record.
pub(super) struct ProcessExitRecord {
    pid: Pid,
    ownership: Mutex<()>,
    state: Mutex<ProcessExitState>,
    ready: Condvar,
    #[cfg(test)]
    pause_publication: AtomicBool,
    #[cfg(test)]
    publication_reached: AtomicBool,
    #[cfg(test)]
    publication_released: AtomicBool,
}

impl ProcessExitRecord {
    fn new(pid: Pid, #[cfg(test)] pause_publication: bool) -> Self {
        Self {
            pid,
            ownership: Mutex::new(()),
            state: Mutex::new(ProcessExitState {
                terminal: None,
                callback: None,
                closed: false,
            }),
            ready: Condvar::new(),
            #[cfg(test)]
            pause_publication: AtomicBool::new(pause_publication),
            #[cfg(test)]
            publication_reached: AtomicBool::new(false),
            #[cfg(test)]
            publication_released: AtomicBool::new(false),
        }
    }

    pub(super) fn lock_ownership(&self) -> Result<MutexGuard<'_, ()>, EngineError> {
        self.ownership
            .lock()
            .map_err(|_| EngineError::ProcessExitOwnershipPoisoned {
                process_id: self.pid,
            })
    }

    pub(super) fn attach_callback(
        &self,
        installation: &Arc<MonitorInstallation>,
        callback: ProcessExitCallback,
    ) -> Result<Option<(ProcessExitCallback, OwnedProcessExitOutcome)>, EngineError> {
        let mut callback = Some(callback);
        let terminal = {
            let mut state = self.lock_state()?;
            if state.closed {
                return Err(EngineError::Runtime {
                    reason: format!("process {} exit record is already closed", self.pid),
                });
            }
            installation.commit();
            if let Some(terminal) = state.terminal.clone() {
                Some(terminal)
            } else {
                state.callback = callback.take();
                None
            }
        };
        Ok(callback.zip(terminal))
    }

    pub(super) fn wait(&self) -> Result<OwnedProcessExitOutcome, EngineError> {
        let mut state = self.lock_state()?;
        loop {
            if let Some(terminal) = state.terminal.clone() {
                return Ok(terminal);
            }
            state = self
                .ready
                .wait(state)
                .map_err(|_| EngineError::ProcessExitStatePoisoned {
                    process_id: self.pid,
                })?;
        }
    }

    pub(super) fn close_without_monitor(&self) -> Result<(), EngineError> {
        let mut state = self.lock_state()?;
        state.closed = true;
        drop(state.callback.take());
        self.ready.notify_all();
        Ok(())
    }

    fn is_terminal(&self) -> Result<bool, EngineError> {
        Ok(self.lock_state()?.terminal.is_some())
    }

    fn publish_and_dispatch(&self, terminal: OwnedProcessExitOutcome) -> Result<(), EngineError> {
        let callback = {
            let mut state = self.lock_state()?;
            if state.terminal.is_some() {
                return Ok(());
            }
            state.terminal = Some(terminal.clone());
            self.ready.notify_all();
            state.callback.take()
        };
        if let Some(callback) = callback {
            callback(terminal);
        }
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, ProcessExitState>, EngineError> {
        self.state
            .lock()
            .map_err(|_| EngineError::ProcessExitStatePoisoned {
                process_id: self.pid,
            })
    }

    #[cfg(test)]
    fn pause_at_publication(&self) {
        if !self.pause_publication.load(Ordering::Acquire) {
            return;
        }
        self.publication_reached.store(true, Ordering::Release);
        while !self.publication_released.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }
}

struct RegistryLifecycle {
    closed: bool,
    /// Outcomes can beat registration because beamr may run a process immediately after spawn.
    pending_registration: HashMap<Pid, OwnedProcessExitOutcome>,
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
    deferred_callbacks: callback::DeferredCallbackDispatcher,
    park_bound: Duration,
    shutdown_timeout: Duration,
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
        let deferred_callbacks = callback::DeferredCallbackDispatcher::new(shutdown_timeout)?;
        let (stopped_sender, stopped) = mpsc::sync_channel(1);
        let registry = Arc::new(Self {
            records: DashMap::new(),
            registered_through: AtomicU64::new(0),
            has_registered: AtomicBool::new(false),
            lifecycle: Mutex::new(RegistryLifecycle {
                closed: false,
                pending_registration: HashMap::new(),
            }),
            stop_drainer: AtomicBool::new(false),
            drainer: Mutex::new(ExitDrainer {
                handle: None,
                stopped,
            }),
            deferred_callbacks,
            park_bound: shutdown_timeout / 2,
            shutdown_timeout,
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

    pub(super) fn register(&self, pid: Pid) -> Result<(), EngineError> {
        #[cfg(test)]
        let pause_publication = self.pause_next_publication.swap(false, Ordering::AcqRel);
        let record = Arc::new(ProcessExitRecord::new(
            pid,
            #[cfg(test)]
            pause_publication,
        ));
        let pending = {
            let mut lifecycle = self.lock_lifecycle()?;
            if lifecycle.closed {
                return Err(EngineError::ShuttingDown);
            }
            match self.records.entry(pid) {
                dashmap::mapref::entry::Entry::Occupied(_) => {
                    return Err(EngineError::Runtime {
                        reason: format!("process {pid} already has a runtime-owned exit record"),
                    });
                }
                dashmap::mapref::entry::Entry::Vacant(entry) => {
                    entry.insert(Arc::clone(&record));
                }
            }
            self.registered_through.fetch_max(pid, Ordering::AcqRel);
            self.has_registered.store(true, Ordering::Release);
            lifecycle.pending_registration.remove(&pid)
        };
        if let Some(pending) = pending {
            record.publish_and_dispatch(pending)?;
        }
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
        self.deferred_callbacks.shutdown()
    }

    pub(super) fn dispatch_deferred_callback(
        &self,
        callback: ProcessExitCallback,
        terminal: OwnedProcessExitOutcome,
    ) -> Result<(), EngineError> {
        self.deferred_callbacks.dispatch(callback, terminal)
    }

    fn process_event(&self, scheduler: &Scheduler, event: ExitEvent) -> Result<(), EngineError> {
        match event {
            ExitEvent::Exited { pid, .. } => match scheduler.take_exit_outcome(pid) {
                Some((reason, result)) => self.publish_taken(scheduler, pid, reason, result),
                None => {
                    // A lag resync can take a concurrently published outcome
                    // immediately before its event reaches the reset stream.
                    // Only that already-cached case is a legal empty take.
                    if self.has_terminal(pid)? {
                        Ok(())
                    } else {
                        Err(EngineError::ProcessExitOutcomeMissingAfterEvent { process_id: pid })
                    }
                }
            },
            ExitEvent::Lagged => {
                #[cfg(test)]
                self.lag_recoveries.fetch_add(1, Ordering::AcqRel);
                tracing::warn!("process exit event subscriber lagged; resynchronizing outcomes");
                self.resynchronize(scheduler)
            }
        }
    }

    fn resynchronize(&self, scheduler: &Scheduler) -> Result<(), EngineError> {
        let pids: Vec<_> = self.records.iter().map(|record| *record.key()).collect();
        for pid in pids {
            if let Some((reason, result)) = scheduler.take_exit_outcome(pid) {
                self.publish_taken(scheduler, pid, reason, result)?;
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
        let outcome = OwnedProcessExitOutcome::Observed(Arc::new(ObservedProcessExit {
            reason,
            result,
            // beamr documents both diagnostics as independent of the durable outcome token.
            execution_error: scheduler.take_exit_error(pid),
            exception: scheduler.take_exit_exception(pid),
        }));
        self.publish_or_defer(pid, outcome)
    }

    fn publish_or_defer(
        &self,
        pid: Pid,
        outcome: OwnedProcessExitOutcome,
    ) -> Result<(), EngineError> {
        let record = {
            let mut lifecycle = self.lock_lifecycle()?;
            if let Some(record) = self.find(pid) {
                Some(record)
            } else {
                lifecycle.pending_registration.insert(pid, outcome.clone());
                None
            }
        };
        if let Some(record) = record {
            record.publish_and_dispatch(outcome)?;
        }
        Ok(())
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
            if let Err(error) = record.publish_and_dispatch(outcome) {
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
