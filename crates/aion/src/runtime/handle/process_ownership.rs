//! Process-spawn exit ownership helpers for [`RuntimeHandle`].

use beamr::process::ExitReason;

use super::{EngineError, Pid, RuntimeHandle};

impl RuntimeHandle {
    pub(super) fn spawn_with_exit_ownership(
        &self,
        spawn: impl FnOnce() -> Result<Pid, EngineError>,
    ) -> Result<Pid, EngineError> {
        let reservation = self.process_exits.reserve_spawn()?;
        let pid = spawn()?;
        if let Err(error) = reservation.register(pid) {
            self.scheduler.terminate_process(pid, ExitReason::Kill);
            if let Err(cleanup_error) = self.finish_process_monitor_cleanup(pid) {
                tracing::error!(pid, %cleanup_error, cause = %error, "spawn rollback cleanup failed after exit ownership setup failed");
                return Err(cleanup_error);
            }
            return Err(error);
        }
        Ok(pid)
    }

    /// Ensure `pid` has a runtime-owned, non-consuming exit outcome record.
    ///
    /// A workflow can run to completion on a scheduler thread between its
    /// spawn and monitor installation. Registration occurs before the `pid` is
    /// returned, so fast exits are read from Aion's permanent cache.
    pub(crate) fn ensure_monitorable_pid(&self, pid: Pid) -> Result<(), EngineError> {
        if self.process_exits.contains(pid) {
            return Ok(());
        }
        if self.process_exits.is_retired(pid) {
            return Err(EngineError::ProcessExitAlreadyTerminal { process_id: pid });
        }
        Err(super::runtime_error(format!(
            "process {pid} was never spawned by this runtime"
        )))
    }

    #[cfg(test)]
    pub(crate) fn process_exit_for_test(
        &self,
        pid: Pid,
    ) -> Result<(ExitReason, beamr::term::Term), EngineError> {
        let observed = self.process_exit_outcome(pid)?;
        self.release_spawn_heaps(pid);
        Ok((observed.reason, observed.result.root()))
    }
}
