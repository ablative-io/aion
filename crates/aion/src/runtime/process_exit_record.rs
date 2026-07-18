//! One-generation cached process-exit records and callback ownership.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use beamr::error::ExecError;
use beamr::ets::OwnedTerm;
use beamr::process::ExitReason;
use beamr::scheduler::OwnedException;

use crate::{EngineError, Pid};

use crate::runtime::monitor::MonitorInstallation;

pub(in crate::runtime) type ProcessExitCallback =
    Box<dyn FnOnce(OwnedProcessExitOutcome) + Send + 'static>;

/// The single captured result of beamr's consuming exit-outcome API.
pub(in crate::runtime) struct ObservedProcessExit {
    pub(in crate::runtime) reason: ExitReason,
    pub(in crate::runtime) result: OwnedTerm,
    pub(in crate::runtime) execution_error: Option<ExecError>,
    pub(in crate::runtime) exception: Option<OwnedException>,
}

/// A defensive failure from the runtime's exclusive exit-event consumer.
#[derive(Clone, Copy)]
pub(in crate::runtime) enum ProcessExitObservationFailure {
    /// An `Exited` event had no takeable outcome and was not a stale post-resync event.
    OutcomeMissingAfterEvent,
    /// beamr disconnected its publisher while the runtime still owned the scheduler.
    EventStreamDisconnected,
}

impl ProcessExitObservationFailure {
    pub(in crate::runtime) fn into_engine_error(self, process_id: Pid) -> EngineError {
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
pub(in crate::runtime) enum OwnedProcessExitOutcome {
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
pub(in crate::runtime) struct ProcessExitRecord {
    pub(in crate::runtime) pid: Pid,
    ownership: Mutex<()>,
    state: Mutex<ProcessExitState>,
    ready: Condvar,
    #[cfg(test)]
    pause_publication: AtomicBool,
    #[cfg(test)]
    pub(in crate::runtime) publication_reached: AtomicBool,
    #[cfg(test)]
    pub(in crate::runtime) publication_released: AtomicBool,
}

impl ProcessExitRecord {
    pub(in crate::runtime) fn new(pid: Pid, #[cfg(test)] pause_publication: bool) -> Self {
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

    pub(in crate::runtime) fn lock_ownership(&self) -> Result<MutexGuard<'_, ()>, EngineError> {
        self.ownership
            .lock()
            .map_err(|_| EngineError::ProcessExitOwnershipPoisoned {
                process_id: self.pid,
            })
    }

    pub(in crate::runtime) fn attach_callback(
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

    pub(in crate::runtime) fn wait(&self) -> Result<OwnedProcessExitOutcome, EngineError> {
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

    pub(in crate::runtime) fn close_without_monitor(&self) -> Result<(), EngineError> {
        let mut state = self.lock_state()?;
        state.closed = true;
        drop(state.callback.take());
        self.ready.notify_all();
        Ok(())
    }

    pub(in crate::runtime) fn is_terminal(&self) -> Result<bool, EngineError> {
        Ok(self.lock_state()?.terminal.is_some())
    }

    pub(in crate::runtime) fn publish(
        &self,
        terminal: OwnedProcessExitOutcome,
    ) -> Result<Option<ProcessExitCallback>, EngineError> {
        let mut state = self.lock_state()?;
        if state.terminal.is_some() {
            return Ok(None);
        }
        state.terminal = Some(terminal);
        self.ready.notify_all();
        Ok(state.callback.take())
    }

    pub(in crate::runtime) fn restore_callback(
        &self,
        callback: ProcessExitCallback,
    ) -> Result<(), EngineError> {
        let mut state = self.lock_state()?;
        if state.callback.is_some() {
            return Err(EngineError::Runtime {
                reason: format!(
                    "process {} exit callback ownership was already restored",
                    self.pid
                ),
            });
        }
        state.callback = Some(callback);
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
    pub(in crate::runtime) fn pause_at_publication(&self) {
        if !self.pause_publication.load(Ordering::Acquire) {
            return;
        }
        self.publication_reached.store(true, Ordering::Release);
        while !self.publication_released.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }
}
