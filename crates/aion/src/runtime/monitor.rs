//! Runtime-owned process monitoring helpers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use beamr::process::ExitReason;
use dashmap::mapref::entry::Entry;

use crate::{EngineError, Pid, RuntimeHandle};

use super::outcome::{self, WorkflowProcessOutcome};

/// Identity retained for the lifetime of one process monitor installation.
pub(super) struct MonitorInstallation {
    committed: AtomicBool,
}

impl MonitorInstallation {
    fn uncommitted() -> Self {
        Self {
            committed: AtomicBool::new(false),
        }
    }
}

/// Typed failure from synchronously aborting an unmonitored process.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UnmonitoredProcessAbortError {
    /// Process removal and shared cleanup did not complete before the bound.
    #[error("process {process_id} did not complete unmonitored abort within {timeout_millis}ms")]
    TimedOut {
        /// Process whose externally requested termination was not fully observed.
        process_id: Pid,
        /// Configured observation bound in milliseconds.
        timeout_millis: u128,
    },
    /// Runtime termination setup or shared cleanup failed.
    #[error(transparent)]
    Engine(EngineError),
}

impl UnmonitoredProcessAbortError {
    pub(crate) fn into_engine_error(self) -> EngineError {
        match self {
            Self::TimedOut {
                process_id,
                timeout_millis,
            } => EngineError::Runtime {
                reason: format!(
                    "process {process_id} did not complete unmonitored abort within {timeout_millis}ms"
                ),
            },
            Self::Engine(error) => error,
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

impl RuntimeHandle {
    /// Install a runtime-owned monitor that invokes `callback` when `pid` exits.
    ///
    /// The callback runs on a dedicated monitor thread outside workflow dirty NIF
    /// execution. The runtime boundary owns the process wait and BEAM term
    /// conversion so lifecycle code never imports beamr types.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when `pid` is neither live nor a
    /// process this runtime previously spawned. A monitored process that
    /// already exited is accepted: its outcome is still observable through
    /// the scheduler's exit tombstone, so the callback fires immediately
    /// instead of the spawn path spuriously failing for fast-completing
    /// workflows.
    pub fn monitor_process<F>(
        self: &Arc<Self>,
        pid: Pid,
        callback: F,
    ) -> Result<ProcessMonitorHandle, EngineError>
    where
        F: FnOnce(Result<WorkflowProcessOutcome, EngineError>) + Send + 'static,
    {
        self.ensure_monitorable_pid(pid)?;
        let installation = self.reserve_monitor_installation(pid)?;
        if !self.is_live(pid) && self.scheduler.peek_exit_reason(pid).is_none() {
            let observation_error = EngineError::Runtime {
                reason: format!(
                    "process {pid} exited and its bounded exit outcome is no longer observable"
                ),
            };
            self.rollback_failed_monitor_installation(pid, &installation)?;
            return Err(observation_error);
        }
        let runtime = Arc::clone(self);
        let monitor_installation = Arc::clone(&installation);
        let monitor = move || {
            while !monitor_installation.committed.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            let process_outcome =
                outcome::workflow_process_outcome(&runtime.scheduler, &runtime.atom_table, pid);
            let monitored_outcome = match runtime.finish_process_monitor_cleanup(pid) {
                Ok(()) => process_outcome,
                Err(error) => {
                    tracing::error!(%error, pid, "workflow activity cleanup failed");
                    Err(error)
                }
            };
            callback(monitored_outcome);
            runtime.release_monitor_installation(pid, &monitor_installation);
        };
        #[cfg(test)]
        let force_monitor_spawn_failure = self.take_monitor_spawn_failure_for_test();
        #[cfg(not(test))]
        let force_monitor_spawn_failure = false;
        let monitor_spawn = if force_monitor_spawn_failure {
            Err(EngineError::Runtime {
                reason: format!(
                    "failed to spawn workflow monitor for process {pid}: forced test failure"
                ),
            })
        } else {
            Self::spawn_monitor_thread(pid, monitor)
        };
        if let Err(error) = monitor_spawn {
            if let Err(rollback_error) =
                self.rollback_failed_monitor_installation(pid, &installation)
            {
                tracing::error!(%error, pid, "workflow monitor spawn failed before bounded rollback");
                return Err(rollback_error);
            }
            return Err(error);
        }
        installation.committed.store(true, Ordering::Release);
        Ok(ProcessMonitorHandle::installed())
    }

    fn reserve_monitor_installation(
        &self,
        pid: Pid,
    ) -> Result<Arc<MonitorInstallation>, EngineError> {
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
        let owns_uncommitted = !installation.committed.load(Ordering::Acquire)
            && self
                .nif_state()
                .monitor_installations
                .get(&pid)
                .is_some_and(|current| Arc::ptr_eq(current.value(), installation));
        if !owns_uncommitted {
            return Ok(());
        }
        self.abort_unmonitored_process_with_installation(pid, Some(Arc::clone(installation)))
            .map_err(UnmonitoredProcessAbortError::into_engine_error)
    }

    /// Terminate and synchronously clean up a process that has no installed monitor.
    ///
    /// A dedicated abort worker owns termination, process-table removal
    /// observation, and the shared monitor cleanup. The caller waits only for
    /// that typed result and is bounded by the runtime readiness timeout. An
    /// already-absent process is cleaned immediately even when its FIFO
    /// tombstone was evicted; this path never consumes a process outcome.
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
        if !self.is_live(pid) {
            let cleanup = self
                .finish_process_monitor_cleanup(pid)
                .map_err(UnmonitoredProcessAbortError::Engine);
            if let Some(installation) = installation {
                self.release_monitor_installation(pid, &installation);
            }
            return cleanup;
        }
        let timeout = self.signal_delivery().ready_timeout;
        let runtime = Arc::clone(self);
        let worker_installation = installation.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name(format!("aion-unmonitored-abort-{pid}"))
            .spawn(move || {
                runtime.scheduler.terminate_process(pid, ExitReason::Kill);
                let cleanup = runtime.finish_process_monitor_cleanup(pid);
                if let Some(installation) = worker_installation {
                    runtime.release_monitor_installation(pid, &installation);
                }
                let _ = sender.send(cleanup);
            });
        if let Err(error) = worker {
            if let Some(installation) = installation {
                self.release_monitor_installation(pid, &installation);
            }
            return Err(UnmonitoredProcessAbortError::Engine(EngineError::Runtime {
                reason: format!("failed to spawn bounded abort worker for process {pid}: {error}"),
            }));
        }
        match receiver.recv_timeout(timeout) {
            Ok(cleanup) => cleanup.map_err(UnmonitoredProcessAbortError::Engine),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err(UnmonitoredProcessAbortError::TimedOut {
                    process_id: pid,
                    timeout_millis: timeout.as_millis(),
                })
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(UnmonitoredProcessAbortError::Engine(EngineError::Runtime {
                    reason: format!("bounded abort worker for process {pid} disconnected"),
                }))
            }
        }
    }

    fn finish_process_monitor_cleanup(&self, pid: Pid) -> Result<(), EngineError> {
        self.release_spawn_heaps(pid);
        self.nif_state().cleanup_process(pid);
        // A Normal workflow exit does not propagate through BEAM links, so
        // any in-VM activity child still running must be torn down here.
        self.kill_in_vm_children(pid);
        // Retained completions racing process death are dropped transactionally
        // before the exact per-pid gate is reaped after process-table removal.
        let activity_cleanup = self.drain_activity_completions(pid);
        self.finish_activity_delivery_cleanup(pid);
        activity_cleanup
    }

    fn spawn_monitor_thread<F>(pid: Pid, monitor: F) -> Result<(), EngineError>
    where
        F: FnOnce() + Send + 'static,
    {
        std::thread::Builder::new()
            .name(format!("aion-workflow-monitor-{pid}"))
            .spawn(monitor)
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to spawn workflow monitor for process {pid}: {error}"),
            })?;
        Ok(())
    }

    /// Test-only monitor installation status probe.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] if the runtime rejects the monitor installation.
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
    pub(crate) fn process_cleanup_observed_for_test(&self, pid: Pid) -> bool {
        self.nif_state().wake_ladder_done(pid, 0)
    }
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
