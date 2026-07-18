//! Constant-cardinality dispatcher for every process-exit callback.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::EngineError;

use super::{OwnedProcessExitOutcome, ProcessExitCallback};

struct ProcessExitCallbackJob {
    callback: ProcessExitCallback,
    terminal: OwnedProcessExitOutcome,
}

pub(super) struct CallbackDispatchFailure {
    pub(super) callback: ProcessExitCallback,
    pub(super) error: EngineError,
}

pub(super) struct ProcessExitCallbackDispatcher {
    sender: Mutex<Option<Sender<ProcessExitCallbackJob>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stopped: Mutex<Receiver<()>>,
    shutdown_timeout: Duration,
}

impl ProcessExitCallbackDispatcher {
    pub(super) fn new(shutdown_timeout: Duration) -> Result<Self, EngineError> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (stopped_sender, stopped) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name(String::from("aion-process-exit-callback"))
            .spawn(move || {
                run_worker(&receiver);
                let _ = stopped_sender.send(());
            })
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to provision process exit callback dispatcher: {error}"),
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            stopped: Mutex::new(stopped),
            shutdown_timeout,
        })
    }

    pub(super) fn dispatch(
        &self,
        callback: ProcessExitCallback,
        terminal: OwnedProcessExitOutcome,
    ) -> Result<(), Box<CallbackDispatchFailure>> {
        let sender = match Self::lock(&self.sender) {
            Ok(sender) => sender,
            Err(error) => return Err(Box::new(CallbackDispatchFailure { callback, error })),
        };
        let Some(sender) = sender.as_ref() else {
            return Err(Box::new(CallbackDispatchFailure {
                callback,
                error: EngineError::ProcessExitCallbackDispatcherUnavailable,
            }));
        };
        let job = ProcessExitCallbackJob { callback, terminal };
        sender.send(job).map_err(|failure| {
            Box::new(CallbackDispatchFailure {
                callback: failure.0.callback,
                error: EngineError::ProcessExitCallbackDispatcherUnavailable,
            })
        })
    }

    pub(super) fn shutdown(&self) -> Result<(), EngineError> {
        Self::lock(&self.sender)?.take();
        if Self::lock(&self.worker)?.is_none() {
            return Ok(());
        }
        match Self::lock(&self.stopped)?.recv_timeout(self.shutdown_timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => {
                return Err(EngineError::ProcessExitCallbackDispatcherShutdownTimedOut {
                    timeout_millis: self.shutdown_timeout.as_millis(),
                });
            }
        }
        if let Some(worker) = Self::lock(&self.worker)?.take() {
            worker.join().map_err(|_| EngineError::Runtime {
                reason: String::from("process exit callback dispatcher terminated unexpectedly"),
            })?;
        }
        Ok(())
    }

    fn lock<T>(state: &Mutex<T>) -> Result<MutexGuard<'_, T>, EngineError> {
        state
            .lock()
            .map_err(|_| EngineError::ProcessExitCallbackDispatcherPoisoned)
    }
}

fn run_worker(receiver: &Receiver<ProcessExitCallbackJob>) {
    while let Ok(job) = receiver.recv() {
        let invoke = move || (job.callback)(job.terminal);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(invoke)).is_err() {
            tracing::error!("process exit callback terminated unexpectedly");
        }
    }
}
