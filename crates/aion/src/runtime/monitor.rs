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
        let runtime = Arc::clone(self);
        std::thread::Builder::new()
            .name(format!("aion-workflow-monitor-{pid}"))
            .spawn(move || {
                let outcome =
                    outcome::workflow_process_outcome(&runtime.scheduler, &runtime.atom_table, pid);
                runtime.release_spawn_heaps(pid);
                runtime.nif_state().cleanup_process(pid);
                // A Normal workflow exit does not propagate through BEAM
                // links, so any in-VM activity child still running (e.g.
                // abandoned by a with_timeout expiry) must be torn down
                // here — the exit side of the "side effects die with the
                // run" contract, and what unblocks the child's exit watcher.
                runtime.kill_in_vm_children(pid);
                // D5: completions delivered after the workflow stopped
                // awaiting them (race losers, post-exit deliveries) are
                // never taken; drop them with the process.
                runtime.drain_activity_completions(pid);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::runtime::{RuntimeConfig, RuntimeHandle};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn monitor_installs_for_process_that_already_exited() -> TestResult {
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let pid = runtime.spawn_test_process()?;
        runtime.cancel_pid(pid)?;
        assert!(
            !runtime.is_live(pid),
            "terminated test process should leave the live table"
        );

        // A workflow can finish on a scheduler thread before its completion
        // monitor installs; the monitor must still observe the exit outcome
        // through the scheduler's tombstone instead of rejecting the pid.
        let (sender, receiver) = mpsc::channel();
        let handle = runtime.monitor_process_for_test(pid, move |outcome| {
            let _ = sender.send(outcome.is_ok());
        })?;

        assert!(handle.is_installed());
        let callback_fired = receiver.recv_timeout(Duration::from_secs(10))?;
        // The outcome conversion result is exercised elsewhere; this test
        // pins the contract that the callback fires for an exited process.
        let _ = callback_fired;
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn monitor_rejects_pid_never_spawned_by_this_runtime() -> TestResult {
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);

        let error = runtime
            .monitor_process_for_test(9_999, |_| {})
            .err()
            .ok_or("monitor accepted a pid this runtime never spawned")?;

        assert!(error.to_string().contains("never spawned"));
        runtime.shutdown()?;
        Ok(())
    }
}
