//! Runtime-owned, non-consuming fan-out records for beamr process exits.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
#[cfg(test)]
use std::time::{Duration, Instant};

use beamr::error::ExecError;
use beamr::ets::OwnedTerm;
use beamr::process::ExitReason;
use beamr::scheduler::{OwnedException, Scheduler};
use dashmap::DashMap;

#[cfg(test)]
use crate::RuntimeHandle;
use crate::{EngineError, Pid};

use super::monitor::MonitorInstallation;

pub(super) type ProcessExitCallback = Box<dyn FnOnce(OwnedProcessExitOutcome) + Send + 'static>;

/// The single captured result of beamr's consuming exit API.
pub(super) struct ObservedProcessExit {
    pub(super) reason: ExitReason,
    pub(super) result: OwnedTerm,
    pub(super) execution_error: Option<ExecError>,
    pub(super) exception: Option<OwnedException>,
}

/// Terminal state cached independently of beamr's bounded tombstone FIFO.
#[derive(Clone)]
pub(super) enum OwnedProcessExitOutcome {
    /// The consuming beamr outcome was captured exactly once.
    Observed(Arc<ObservedProcessExit>),
    /// The process was already dead after its tombstone had become unavailable.
    DeadAndUnavailable { process_id: Pid },
}

struct ProcessExitState {
    terminal: Option<OwnedProcessExitOutcome>,
    callback: Option<ProcessExitCallback>,
    stop_observer: bool,
}

/// One `pid` generation's exit record and tracked observer.
pub(super) struct ProcessExitRecord {
    pid: Pid,
    ownership: Mutex<()>,
    state: Mutex<ProcessExitState>,
    ready: Condvar,
    observer: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    pause_observer: AtomicBool,
    #[cfg(test)]
    observer_entered: AtomicBool,
    #[cfg(test)]
    observer_released: AtomicBool,
    #[cfg(test)]
    force_unavailable: AtomicBool,
    #[cfg(test)]
    pause_publication: AtomicBool,
    #[cfg(test)]
    publication_reached: AtomicBool,
    #[cfg(test)]
    publication_released: AtomicBool,
}

impl ProcessExitRecord {
    fn new(
        pid: Pid,
        #[cfg(test)] pause_observer: bool,
        #[cfg(test)] pause_publication: bool,
    ) -> Self {
        Self {
            pid,
            ownership: Mutex::new(()),
            state: Mutex::new(ProcessExitState {
                terminal: None,
                callback: None,
                stop_observer: false,
            }),
            ready: Condvar::new(),
            observer: Mutex::new(None),
            #[cfg(test)]
            pause_observer: AtomicBool::new(pause_observer),
            #[cfg(test)]
            observer_entered: AtomicBool::new(false),
            #[cfg(test)]
            observer_released: AtomicBool::new(false),
            #[cfg(test)]
            force_unavailable: AtomicBool::new(false),
            #[cfg(test)]
            pause_publication: AtomicBool::new(pause_publication),
            #[cfg(test)]
            publication_reached: AtomicBool::new(false),
            #[cfg(test)]
            publication_released: AtomicBool::new(false),
        }
    }

    fn set_observer(&self, observer: JoinHandle<()>) -> Result<(), EngineError> {
        let mut slot = self.lock_observer()?;
        *slot = Some(observer);
        Ok(())
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
    ) -> Result<(), EngineError> {
        let mut state = self.lock_state()?;
        if state.stop_observer {
            return Err(EngineError::Runtime {
                reason: format!("process {} exit observer is already closed", self.pid),
            });
        }
        installation.commit();
        state.callback = Some(callback);
        self.ready.notify_all();
        Ok(())
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
        state.stop_observer = true;
        drop(state.callback.take());
        self.ready.notify_all();
        Ok(())
    }

    fn observe(self: &Arc<Self>, scheduler: &Scheduler) {
        #[cfg(test)]
        self.pause_before_observation();
        let forced_unavailable = {
            #[cfg(test)]
            {
                self.force_unavailable.load(Ordering::Acquire)
            }
            #[cfg(not(test))]
            {
                false
            }
        };
        let terminal = if scheduler.process_table().get(self.pid).is_none()
            && (forced_unavailable || scheduler.peek_exit_reason(self.pid).is_none())
        {
            OwnedProcessExitOutcome::DeadAndUnavailable {
                process_id: self.pid,
            }
        } else {
            let (reason, result) = scheduler.run_until_exit(self.pid);
            OwnedProcessExitOutcome::Observed(Arc::new(ObservedProcessExit {
                reason,
                result,
                execution_error: scheduler.take_exit_error(self.pid),
                exception: scheduler.take_exit_exception(self.pid),
            }))
        };
        if let Err(error) = self.publish_and_dispatch(terminal) {
            tracing::error!(pid = self.pid, %error, "process exit outcome publication failed");
        }
    }

    fn publish_and_dispatch(&self, terminal: OwnedProcessExitOutcome) -> Result<(), EngineError> {
        let callback = {
            let mut state = self.lock_state()?;
            state.terminal = Some(terminal.clone());
            self.ready.notify_all();
            loop {
                if let Some(callback) = state.callback.take() {
                    break Some(callback);
                }
                if state.stop_observer {
                    break None;
                }
                state =
                    self.ready
                        .wait(state)
                        .map_err(|_| EngineError::ProcessExitStatePoisoned {
                            process_id: self.pid,
                        })?;
            }
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

    fn lock_observer(&self) -> Result<MutexGuard<'_, Option<JoinHandle<()>>>, EngineError> {
        self.observer
            .lock()
            .map_err(|_| EngineError::ProcessExitObserverPoisoned {
                process_id: self.pid,
            })
    }

    fn join(&self) -> Result<(), EngineError> {
        let observer = self.lock_observer()?.take();
        if let Some(observer) = observer {
            observer.join().map_err(|_| EngineError::Runtime {
                reason: format!("process {} exit observer terminated unexpectedly", self.pid),
            })?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn pause_before_observation(&self) {
        if !self.pause_observer.load(Ordering::Acquire) {
            return;
        }
        self.observer_entered.store(true, Ordering::Release);
        while !self.observer_released.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
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

/// Index of active owned exit records.
pub(super) struct ProcessExitRegistry {
    records: DashMap<Pid, Arc<ProcessExitRecord>>,
    /// Highest pid registered by this runtime. beamr allocates pids
    /// monotonically and never reuses them, so this compact watermark lets a
    /// late duplicate resolve as already-terminal after its heavy record is
    /// retired without retaining another per-pid map.
    registered_through: AtomicU64,
    has_registered: AtomicBool,
    closed: Mutex<bool>,
    #[cfg(test)]
    pause_next_observer: AtomicBool,
    #[cfg(test)]
    pause_next_publication: AtomicBool,
}

impl ProcessExitRegistry {
    pub(super) fn new() -> Self {
        Self {
            records: DashMap::new(),
            registered_through: AtomicU64::new(0),
            has_registered: AtomicBool::new(false),
            closed: Mutex::new(false),
            #[cfg(test)]
            pause_next_observer: AtomicBool::new(false),
            #[cfg(test)]
            pause_next_publication: AtomicBool::new(false),
        }
    }

    pub(super) fn register(&self, scheduler: Arc<Scheduler>, pid: Pid) -> Result<(), EngineError> {
        let closed = self
            .closed
            .lock()
            .map_err(|_| EngineError::ProcessExitRegistryPoisoned)?;
        if *closed {
            return Err(EngineError::ShuttingDown);
        }
        #[cfg(test)]
        let pause_observer = self.pause_next_observer.swap(false, Ordering::AcqRel);
        #[cfg(test)]
        let pause_publication = self.pause_next_publication.swap(false, Ordering::AcqRel);
        let record = Arc::new(ProcessExitRecord::new(
            pid,
            #[cfg(test)]
            pause_observer,
            #[cfg(test)]
            pause_publication,
        ));
        let observer_record = Arc::clone(&record);
        let observer = std::thread::Builder::new()
            .name(format!("aion-process-exit-{pid}"))
            .spawn(move || observer_record.observe(&scheduler))
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to establish exit ownership for process {pid}: {error}"),
            })?;
        record.set_observer(observer)?;
        if self.records.insert(pid, record).is_some() {
            return Err(EngineError::Runtime {
                reason: format!("process {pid} already has a runtime-owned exit record"),
            });
        }
        self.registered_through.fetch_max(pid, Ordering::AcqRel);
        self.has_registered.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn get(&self, pid: Pid) -> Result<Arc<ProcessExitRecord>, EngineError> {
        self.records
            .get(&pid)
            .map(|record| Arc::clone(record.value()))
            .ok_or_else(|| {
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
        let mut closed = self
            .closed
            .lock()
            .map_err(|_| EngineError::ProcessExitRegistryPoisoned)?;
        *closed = true;
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
        let records: Vec<_> = self
            .records
            .iter()
            .map(|record| Arc::clone(record.value()))
            .collect();
        for record in &records {
            record.close_without_monitor()?;
        }
        for record in records {
            record.join()?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn pause_next_observer(&self) {
        self.pause_next_observer.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn force_unavailable_and_release(&self, pid: Pid) -> Result<(), EngineError> {
        let record = self.get(pid)?;
        record.force_unavailable.store(true, Ordering::Release);
        record.observer_released.store(true, Ordering::Release);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn wait_for_observer_pause(
        &self,
        pid: Pid,
        timeout: Duration,
    ) -> Result<(), EngineError> {
        let record = self.get(pid)?;
        wait_for_flag(
            &record.observer_entered,
            timeout,
            format!("process {pid} exit observer did not reach its test pause"),
        )
    }

    #[cfg(test)]
    pub(super) fn pause_next_publication(&self) {
        self.pause_next_publication.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn pause_at_publication(&self, pid: Pid) -> Result<(), EngineError> {
        self.get(pid)?.pause_at_publication();
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn wait_for_publication_pause(
        &self,
        pid: Pid,
        timeout: Duration,
    ) -> Result<(), EngineError> {
        let record = self.get(pid)?;
        wait_for_flag(
            &record.publication_reached,
            timeout,
            format!("process {pid} start publication did not reach its test pause"),
        )
    }

    #[cfg(test)]
    pub(super) fn release_publication(&self, pid: Pid) -> Result<(), EngineError> {
        self.get(pid)?
            .publication_released
            .store(true, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
impl RuntimeHandle {
    pub(crate) fn pause_next_exit_observer_for_test(&self) {
        self.process_exits.pause_next_observer();
    }

    pub(crate) fn wait_for_exit_observer_pause_for_test(
        &self,
        pid: Pid,
    ) -> Result<(), EngineError> {
        self.process_exits
            .wait_for_observer_pause(pid, self.signal_delivery().ready_timeout)
    }

    pub(crate) fn force_exit_outcome_unavailable_for_test(
        &self,
        pid: Pid,
    ) -> Result<(), EngineError> {
        self.process_exits.force_unavailable_and_release(pid)
    }

    pub(crate) fn pause_next_start_publication_for_test(&self) {
        self.process_exits.pause_next_publication();
    }

    pub(crate) fn pause_at_start_publication_for_test(&self, pid: Pid) -> Result<(), EngineError> {
        self.process_exits.pause_at_publication(pid)
    }

    pub(crate) fn wait_for_start_publication_pause_for_test(
        &self,
        pid: Pid,
    ) -> Result<(), EngineError> {
        self.process_exits
            .wait_for_publication_pause(pid, self.signal_delivery().ready_timeout)
    }

    pub(crate) fn release_start_publication_for_test(&self, pid: Pid) -> Result<(), EngineError> {
        self.process_exits.release_publication(pid)
    }

    pub(crate) fn shutdown_cleanup_executor_for_test(&self) -> Result<(), EngineError> {
        self.cleanup_executor.shutdown()
    }

    pub(crate) fn observe_native_entry_for_test(&self, pid: Pid) {
        self.nif_state().observe_native_entry(pid);
    }
}

#[cfg(test)]
fn wait_for_flag(flag: &AtomicBool, timeout: Duration, reason: String) -> Result<(), EngineError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if flag.load(Ordering::Acquire) {
            return Ok(());
        }
        std::thread::yield_now();
    }
    Err(EngineError::Runtime { reason })
}
