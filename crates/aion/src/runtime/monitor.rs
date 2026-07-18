//! Runtime-owned process exit monitoring and cleanup orchestration.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use beamr::process::ExitReason;
use dashmap::mapref::entry::Entry;

use crate::{EngineError, Pid, RuntimeHandle};

use super::cleanup_executor::CleanupSubmitError;
use super::outcome::{self, WorkflowProcessOutcome};
use super::process_exit::{ObservedProcessExit, OwnedProcessExitOutcome};

/// Identity retained until one committed monitor callback completes.
pub(super) struct MonitorInstallation {
    committed: AtomicBool,
}

impl MonitorInstallation {
    fn uncommitted() -> Self {
        Self {
            committed: AtomicBool::new(false),
        }
    }

    pub(super) fn commit(&self) {
        self.committed.store(true, Ordering::Release);
    }
}

/// Typed failure from synchronously requesting an unmonitored-process abort.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UnmonitoredProcessAbortError {
    /// Process cleanup did not complete before the caller's observation bound.
    #[error("process {process_id} did not complete unmonitored abort within {timeout_millis}ms")]
    TimedOut {
        /// Process whose termination remained owned by the runtime job.
        process_id: Pid,
        /// Configured observation bound in milliseconds.
        timeout_millis: u128,
    },
    /// The runtime cleanup executor had already closed.
    #[error("process cleanup executor is unavailable for process {process_id}")]
    ExecutorUnavailable {
        /// Process retained by the terminal abort identity.
        process_id: Pid,
    },
    /// The bounded cleanup executor queue had no capacity for a distinct job.
    #[error("process cleanup executor is exhausted for process {process_id}")]
    ExecutorExhausted {
        /// Process retained by the terminal abort identity.
        process_id: Pid,
    },
    /// The cleanup executor's ownership lock was poisoned.
    #[error("process cleanup executor state is poisoned for process {process_id}")]
    ExecutorPoisoned {
        /// Process retained by the terminal abort identity.
        process_id: Pid,
    },
    /// A completion monitor already owns this process generation.
    #[error("process {process_id} already has a completion monitor owner")]
    MonitorInstalled {
        /// Process the abort refused to terminate.
        process_id: Pid,
    },
    /// The per-process monitor/abort ownership gate was poisoned.
    #[error("process exit ownership gate for process {process_id} was poisoned")]
    OwnershipPoisoned {
        /// Process whose ownership could not be serialized.
        process_id: Pid,
    },
    /// An abort job's identity state was poisoned.
    #[error("unmonitored abort state for process {process_id} was poisoned")]
    StatePoisoned {
        /// Process whose abort state could not be observed.
        process_id: Pid,
    },
    /// Runtime cleanup failed after the job acquired execution ownership.
    #[error("process {process_id} cleanup failed: {reason}")]
    CleanupFailed {
        /// Process whose shared cleanup failed.
        process_id: Pid,
        /// Typed engine failure rendered for repeatable fan-out reads.
        reason: String,
    },
}

impl UnmonitoredProcessAbortError {
    pub(crate) fn into_engine_error(self) -> EngineError {
        EngineError::Runtime {
            reason: self.to_string(),
        }
    }
}

/// Handle returned after installing a workflow process monitor.
#[derive(Clone)]
pub struct ProcessMonitorHandle {
    installed: Arc<AtomicBool>,
}

impl ProcessMonitorHandle {
    fn installed() -> Self {
        Self {
            installed: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Returns whether the runtime accepted monitor installation.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.installed.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
enum AbortJobTerminal {
    Succeeded,
    CleanupFailed(String),
}

enum AbortJobPhase {
    Running,
    Finalizing,
    Complete(AbortJobTerminal),
}

struct AbortJobState {
    phase: AbortJobPhase,
    installation: Option<Arc<MonitorInstallation>>,
}

/// One runtime-owned abort identity for a `pid` generation.
pub(super) struct UnmonitoredProcessAbortJob {
    pid: Pid,
    state: Mutex<AbortJobState>,
    ready: Condvar,
}

impl UnmonitoredProcessAbortJob {
    fn new(pid: Pid, installation: Option<Arc<MonitorInstallation>>) -> Self {
        Self {
            pid,
            state: Mutex::new(AbortJobState {
                phase: AbortJobPhase::Running,
                installation,
            }),
            ready: Condvar::new(),
        }
    }

    fn attach_installation(
        &self,
        installation: Option<Arc<MonitorInstallation>>,
    ) -> Result<(), UnmonitoredProcessAbortError> {
        let Some(installation) = installation else {
            return Ok(());
        };
        let mut state = self.lock_state()?;
        if state.installation.is_none() {
            state.installation = Some(installation);
        }
        Ok(())
    }

    fn complete_cleanup(
        self: &Arc<Self>,
        runtime: &RuntimeHandle,
        record: Option<&Arc<super::process_exit::ProcessExitRecord>>,
        cleanup: Result<(), EngineError>,
    ) -> Result<(), UnmonitoredProcessAbortError> {
        let (installation, terminal) = {
            let mut state = self.lock_state()?;
            let terminal = match cleanup {
                Ok(()) => AbortJobTerminal::Succeeded,
                Err(error) => AbortJobTerminal::CleanupFailed(error.to_string()),
            };
            state.phase = AbortJobPhase::Finalizing;
            (state.installation.take(), terminal)
        };
        let ownership = record
            .map(|record| record.lock_ownership())
            .transpose()
            .map_err(|_| UnmonitoredProcessAbortError::OwnershipPoisoned {
                process_id: self.pid,
            })?;
        if let Some(installation) = installation {
            runtime.release_monitor_installation(self.pid, &installation);
        }
        if let Some(record) = record {
            runtime.process_exits.retire(self.pid, record);
        }
        let mut state = self.lock_state()?;
        state.phase = AbortJobPhase::Complete(terminal);
        if let Entry::Occupied(entry) = runtime.abort_jobs.entry(self.pid) {
            if Arc::ptr_eq(entry.get(), self) {
                entry.remove();
            }
        }
        self.ready.notify_all();
        drop(state);
        drop(ownership);
        Ok(())
    }

    fn wait(&self, timeout: std::time::Duration) -> Result<(), UnmonitoredProcessAbortError> {
        let state = self.lock_state()?;
        let (state, wait) = self
            .ready
            .wait_timeout_while(state, timeout, |state| {
                matches!(
                    state.phase,
                    AbortJobPhase::Running | AbortJobPhase::Finalizing
                )
            })
            .map_err(|_| UnmonitoredProcessAbortError::StatePoisoned {
                process_id: self.pid,
            })?;
        match &state.phase {
            AbortJobPhase::Running | AbortJobPhase::Finalizing if wait.timed_out() => {
                Err(UnmonitoredProcessAbortError::TimedOut {
                    process_id: self.pid,
                    timeout_millis: timeout.as_millis(),
                })
            }
            AbortJobPhase::Running | AbortJobPhase::Finalizing => {
                Err(UnmonitoredProcessAbortError::StatePoisoned {
                    process_id: self.pid,
                })
            }
            AbortJobPhase::Complete(terminal) => terminal.result(self.pid),
        }
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, AbortJobState>, UnmonitoredProcessAbortError> {
        self.state
            .lock()
            .map_err(|_| UnmonitoredProcessAbortError::StatePoisoned {
                process_id: self.pid,
            })
    }
}

impl AbortJobTerminal {
    fn result(&self, pid: Pid) -> Result<(), UnmonitoredProcessAbortError> {
        match self {
            Self::Succeeded => Ok(()),
            Self::CleanupFailed(reason) => Err(UnmonitoredProcessAbortError::CleanupFailed {
                process_id: pid,
                reason: reason.clone(),
            }),
        }
    }
}

impl RuntimeHandle {
    /// Install one completion callback against the `pid`'s owned exit record.
    ///
    /// # Errors
    ///
    /// Returns a typed runtime error for unknown pids, duplicate committed
    /// installations, or an abort already owning this `pid` generation.
    pub fn monitor_process<F>(
        self: &Arc<Self>,
        pid: Pid,
        callback: F,
    ) -> Result<ProcessMonitorHandle, EngineError>
    where
        F: FnOnce(Result<WorkflowProcessOutcome, EngineError>) + Send + 'static,
    {
        self.ensure_monitorable_pid(pid)?;
        let record = self.process_exits.get(pid)?;
        let ownership = record.lock_ownership()?;
        if !self.process_exits.is_current(pid, &record) {
            return Err(EngineError::ProcessExitAlreadyTerminal { process_id: pid });
        }
        let installation = self.reserve_monitor_installation(pid)?;
        #[cfg(test)]
        if self.take_monitor_installation_failure_for_test() {
            let error = EngineError::Runtime {
                reason: format!(
                    "failed to install workflow monitor for process {pid}: forced test failure"
                ),
            };
            drop(ownership);
            self.rollback_failed_monitor_installation(pid, &installation)?;
            return Err(error);
        }

        let runtime = Arc::clone(self);
        let callback_record = Arc::clone(&record);
        let callback_installation = Arc::clone(&installation);
        let completion = Box::new(move |owned| {
            let process_outcome =
                outcome::workflow_outcome_from_owned_exit(&runtime.atom_table, pid, &owned);
            let monitored_outcome = match runtime.finish_process_monitor_cleanup(pid) {
                Ok(()) => process_outcome,
                Err(error) => {
                    tracing::error!(%error, pid, "workflow activity cleanup failed");
                    Err(error)
                }
            };
            let callback_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                callback(monitored_outcome);
            }));
            match callback_record.lock_ownership() {
                Ok(retirement) => {
                    runtime.release_monitor_installation(pid, &callback_installation);
                    runtime.process_exits.retire(pid, &callback_record);
                    drop(retirement);
                }
                Err(error) => {
                    tracing::error!(pid, %error, "failed to retire completed process monitor");
                }
            }
            if let Err(panic) = callback_result {
                std::panic::resume_unwind(panic);
            }
        });
        let deferred_dispatch = match record.attach_callback(&installation, completion) {
            Ok(deferred_dispatch) => deferred_dispatch,
            Err(error) => {
                drop(ownership);
                self.rollback_failed_monitor_installation(pid, &installation)?;
                return Err(error);
            }
        };
        drop(ownership);
        if let Some((callback, terminal)) = deferred_dispatch {
            self.process_exits
                .dispatch_deferred_callback(callback, terminal)?;
        }
        Ok(ProcessMonitorHandle::installed())
    }

    fn reserve_monitor_installation(
        &self,
        pid: Pid,
    ) -> Result<Arc<MonitorInstallation>, EngineError> {
        if self.abort_jobs.contains_key(&pid) {
            return Err(EngineError::Runtime {
                reason: format!("process {pid} already has an abort job"),
            });
        }
        match self.nif_state().monitor_installations.entry(pid) {
            Entry::Vacant(entry) => {
                let installation = Arc::new(MonitorInstallation::uncommitted());
                entry.insert(Arc::clone(&installation));
                Ok(installation)
            }
            Entry::Occupied(_) => Err(EngineError::Runtime {
                reason: format!("process {pid} already has a completion monitor installation"),
            }),
        }
    }

    fn release_monitor_installation(&self, pid: Pid, installation: &Arc<MonitorInstallation>) {
        if let Entry::Occupied(entry) = self.nif_state().monitor_installations.entry(pid) {
            if Arc::ptr_eq(entry.get(), installation) {
                entry.remove();
            }
        }
    }

    fn rollback_failed_monitor_installation(
        self: &Arc<Self>,
        pid: Pid,
        installation: &Arc<MonitorInstallation>,
    ) -> Result<(), EngineError> {
        if installation.committed.load(Ordering::Acquire) {
            return Ok(());
        }
        self.abort_unmonitored_process_with_installation(pid, Some(Arc::clone(installation)))
            .map_err(UnmonitoredProcessAbortError::into_engine_error)
    }

    /// Terminate and synchronously observe cleanup of an unmonitored process.
    pub(crate) fn abort_unmonitored_process(
        self: &Arc<Self>,
        pid: Pid,
    ) -> Result<(), UnmonitoredProcessAbortError> {
        self.abort_unmonitored_process_with_installation(pid, None)
    }

    fn abort_unmonitored_process_with_installation(
        self: &Arc<Self>,
        pid: Pid,
        installation: Option<Arc<MonitorInstallation>>,
    ) -> Result<(), UnmonitoredProcessAbortError> {
        if self.process_exits.is_retired(pid) {
            return Ok(());
        }
        let record = self.process_exits.find(pid);
        if record.is_none() && self.process_exits.is_retired(pid) {
            return Ok(());
        }
        let ownership = record
            .as_ref()
            .map(|record| record.lock_ownership())
            .transpose()
            .map_err(|_| UnmonitoredProcessAbortError::OwnershipPoisoned { process_id: pid })?;
        if record
            .as_ref()
            .is_some_and(|record| !self.process_exits.is_current(pid, record))
        {
            return Ok(());
        }
        let job = match self.abort_jobs.entry(pid) {
            Entry::Occupied(entry) => {
                let job = Arc::clone(entry.get());
                drop(entry);
                job.attach_installation(installation)?;
                job
            }
            Entry::Vacant(entry) => {
                let owns_uncommitted_installation = installation.as_ref().is_some_and(|expected| {
                    !expected.committed.load(Ordering::Acquire)
                        && self
                            .nif_state()
                            .monitor_installations
                            .get(&pid)
                            .is_some_and(|current| Arc::ptr_eq(current.value(), expected))
                });
                if self.nif_state().monitor_installations.contains_key(&pid)
                    && !owns_uncommitted_installation
                {
                    return Err(UnmonitoredProcessAbortError::MonitorInstalled { process_id: pid });
                }
                let refused_installation = installation.clone();
                let job = Arc::new(UnmonitoredProcessAbortJob::new(pid, installation));
                let runtime = Arc::clone(self);
                let worker_job = Arc::clone(&job);
                let worker_record = record.clone();
                let submission = self.cleanup_executor.submit(Box::new(move || {
                    if runtime.is_live(pid) {
                        runtime.scheduler.terminate_process(pid, ExitReason::Kill);
                    }
                    let mut cleanup = runtime.finish_process_monitor_cleanup(pid);
                    if let Some(record) = worker_record.as_ref() {
                        if let Err(error) = record.wait() {
                            if cleanup.is_ok() {
                                cleanup = Err(error);
                            }
                        }
                        if let Err(error) = record.close_without_monitor() {
                            if cleanup.is_ok() {
                                cleanup = Err(error);
                            }
                        }
                    }
                    if let Err(error) =
                        worker_job.complete_cleanup(&runtime, worker_record.as_ref(), cleanup)
                    {
                        tracing::error!(pid, %error, "failed to publish process abort completion");
                    }
                }));
                if let Err(error) = submission {
                    if let Some(installation) = refused_installation {
                        self.release_monitor_installation(pid, &installation);
                    }
                    return Err(match error {
                        CleanupSubmitError::Unavailable => {
                            UnmonitoredProcessAbortError::ExecutorUnavailable { process_id: pid }
                        }
                        CleanupSubmitError::Exhausted => {
                            UnmonitoredProcessAbortError::ExecutorExhausted { process_id: pid }
                        }
                        CleanupSubmitError::Poisoned => {
                            UnmonitoredProcessAbortError::ExecutorPoisoned { process_id: pid }
                        }
                    });
                }
                entry.insert(Arc::clone(&job));
                job
            }
        };
        drop(ownership);
        job.wait(self.signal_delivery().ready_timeout)
    }

    #[cfg(test)]
    pub(super) fn process_exit_outcome(
        &self,
        pid: Pid,
    ) -> Result<Arc<ObservedProcessExit>, EngineError> {
        match self.process_exits.get(pid)?.wait()? {
            OwnedProcessExitOutcome::Observed(observed) => Ok(observed),
            OwnedProcessExitOutcome::ObservationFailed {
                process_id,
                failure,
            } => Err(failure.into_engine_error(process_id)),
        }
    }

    pub(super) fn activity_process_exit_outcome(
        &self,
        pid: Pid,
    ) -> Result<Arc<ObservedProcessExit>, EngineError> {
        let record = self.process_exits.get(pid)?;
        let outcome = match record.wait()? {
            OwnedProcessExitOutcome::Observed(observed) => Ok(observed),
            OwnedProcessExitOutcome::ObservationFailed {
                process_id,
                failure,
            } => Err(failure.into_engine_error(process_id)),
        };
        record.close_without_monitor()?;
        let retirement = record.lock_ownership()?;
        self.process_exits.retire(pid, &record);
        drop(retirement);
        outcome
    }

    pub(super) fn finish_process_monitor_cleanup(&self, pid: Pid) -> Result<(), EngineError> {
        self.release_spawn_heaps(pid);
        self.nif_state().cleanup_process(pid);
        self.kill_in_vm_children(pid);
        let activity_cleanup = self.drain_activity_completions(pid);
        self.finish_activity_delivery_cleanup(pid);
        activity_cleanup
    }

    /// Test-only monitor installation status probe.
    ///
    /// # Errors
    ///
    /// Returns the same typed installation errors as [`Self::monitor_process`].
    #[cfg(test)]
    pub fn monitor_process_for_test<F>(
        self: &Arc<Self>,
        pid: Pid,
        callback: F,
    ) -> Result<ProcessMonitorHandle, EngineError>
    where
        F: FnOnce(Result<WorkflowProcessOutcome, EngineError>) + Send + 'static,
    {
        self.monitor_process(pid, callback)
    }

    #[cfg(test)]
    pub(crate) fn process_cleanup_started_for_test(&self, pid: Pid) -> bool {
        self.nif_state().process_cleanup_started(pid)
    }

    #[cfg(test)]
    pub(crate) fn process_cleanup_complete_for_test(&self, pid: Pid) -> bool {
        self.process_cleanup_started_for_test(pid)
            && !self.is_live(pid)
            && !self.abort_jobs.contains_key(&pid)
            && !self.nif_state().monitor_installations.contains_key(&pid)
            && !self.process_exits.contains(pid)
    }
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
