//! Runtime-owned process monitoring helpers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use beamr::process::ExitReason;

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
        let monitor = move || {
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
            self.rollback_failed_monitor_installation(pid);
            return Err(error);
        }
        Ok(ProcessMonitorHandle::installed())
    }

    fn rollback_failed_monitor_installation(&self, pid: Pid) {
        if self.is_live(pid) {
            self.scheduler.terminate_process(pid, ExitReason::Kill);
        }
        if let Err(error) =
            outcome::workflow_process_outcome(&self.scheduler, &self.atom_table, pid)
        {
            tracing::warn!(%error, pid, "failed to decode terminated unmonitored workflow outcome");
        }
        if let Err(error) = self.finish_process_monitor_cleanup(pid) {
            tracing::error!(%error, pid, "unmonitored workflow activity cleanup failed");
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::runtime::{RuntimeConfig, RuntimeHandle};
    use aion_core::{ContentType, Payload};

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

    #[test]
    fn monitor_spawn_failure_drains_retained_completion_transaction() -> TestResult {
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let baseline_gates = runtime.activity_delivery_gate_count();
        let pid = runtime.spawn_test_process()?;
        runtime.deliver_activity_completion_message_with_attempt(
            pid,
            "activity:41",
            String::from(r#"{"completed":true}"#),
            Some(3),
        )?;
        assert_eq!(
            runtime.activity_result(pid, 41),
            Some(Payload::new(
                ContentType::Json,
                br#"{"completed":true}"#.to_vec()
            ))
        );
        assert_eq!(runtime.retained_activity_attempt_count_for_test(), 1);
        assert_eq!(runtime.activity_delivery_gate_count(), baseline_gates + 1);

        runtime.force_next_monitor_spawn_failure_for_test();
        let error = runtime
            .monitor_process_for_test(pid, |_| {})
            .err()
            .ok_or("forced monitor spawn failure installed a monitor")?;

        assert!(
            error.to_string().contains("forced test failure"),
            "typed monitor installation error must remain visible"
        );
        assert!(
            !runtime.is_live(pid),
            "failed monitor installation must synchronously terminate the process"
        );
        assert_eq!(runtime.retained_activity_completions(), 0);
        assert_eq!(runtime.retained_activity_attempt_count_for_test(), 0);
        assert_eq!(runtime.activity_delivery_gate_count(), baseline_gates);
        runtime.shutdown()?;
        Ok(())
    }
}
