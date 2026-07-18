//! Test-only control and observation seams for the process-exit drainer.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::{EngineError, Pid, RuntimeHandle};

use super::{AtomicBool, ProcessExitRegistry};

impl ProcessExitRegistry {
    pub(super) fn pause_next_registration(&self) {
        self.registration_released.store(false, Ordering::Release);
        self.registration_reached.store(0, Ordering::Release);
        self.pause_next_registration.store(true, Ordering::Release);
    }

    pub(super) fn wait_for_registration_pause(
        &self,
        timeout: Duration,
    ) -> Result<Pid, EngineError> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let pid = self.registration_reached.load(Ordering::Acquire);
            if pid != 0 {
                return Ok(pid);
            }
            std::thread::yield_now();
        }
        Err(EngineError::Runtime {
            reason: String::from("spawn did not reach the exit-record registration pause"),
        })
    }

    pub(super) fn release_registration(&self) {
        self.registration_released.store(true, Ordering::Release);
    }

    pub(super) fn unobserved_children_for_test(&self) -> Result<usize, EngineError> {
        Ok(self.lock_lifecycle()?.unobserved_children.len())
    }

    pub(super) fn pause_next_publication(&self) {
        self.pause_next_publication.store(true, Ordering::Release);
    }

    pub(super) fn pause_at_publication(&self, pid: Pid) -> Result<(), EngineError> {
        self.get(pid)?.pause_at_publication();
        Ok(())
    }

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

    pub(super) fn release_publication(&self, pid: Pid) -> Result<(), EngineError> {
        self.get(pid)?
            .publication_released
            .store(true, Ordering::Release);
        Ok(())
    }

    pub(in crate::runtime) fn pause_for_test(&self) {
        self.drainer_paused.store(false, Ordering::Release);
        self.pause_drainer.store(true, Ordering::Release);
    }

    pub(in crate::runtime) fn wait_for_pause_for_test(
        &self,
        timeout: Duration,
    ) -> Result<(), EngineError> {
        wait_for_flag(
            &self.drainer_paused,
            timeout,
            String::from("process exit drainer did not reach its test pause"),
        )
    }

    pub(in crate::runtime) fn release_for_test(&self) {
        self.pause_drainer.store(false, Ordering::Release);
    }

    pub(super) fn lag_recoveries_for_test(&self) -> u64 {
        self.lag_recoveries.load(Ordering::Acquire)
    }

    pub(in crate::runtime) fn drainer_joined_for_test(&self) -> Result<bool, EngineError> {
        Ok(self.lock_drainer()?.handle.is_none())
    }
}

impl RuntimeHandle {
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
