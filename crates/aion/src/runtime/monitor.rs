//! Runtime-owned process monitoring helpers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{EngineError, Pid, RuntimeHandle};

use super::outcome::{self, WorkflowProcessOutcome};

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
    /// Returns [`EngineError::Runtime`] when `pid` is not live at installation.
    pub fn monitor_process<F>(
        self: &Arc<Self>,
        pid: Pid,
        callback: F,
    ) -> Result<ProcessMonitorHandle, EngineError>
    where
        F: FnOnce(Result<WorkflowProcessOutcome, EngineError>) + Send + 'static,
    {
        self.ensure_live_pid(pid)?;
        let runtime = Arc::clone(self);
        std::thread::Builder::new()
            .name(format!("aion-workflow-monitor-{pid}"))
            .spawn(move || {
                let outcome =
                    outcome::workflow_process_outcome(&runtime.scheduler, &runtime.atom_table, pid);
                runtime.release_spawn_heaps(pid);
                callback(outcome);
            })
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to spawn workflow monitor for process {pid}: {error}"),
            })?;
        Ok(ProcessMonitorHandle::installed())
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
}
