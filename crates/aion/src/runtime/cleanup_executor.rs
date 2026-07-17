//! Runtime-provisioned executor for process abort and cleanup jobs.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::EngineError;

type CleanupJob = Box<dyn FnOnce() + Send + 'static>;

/// Typed failure to submit work to the runtime-owned cleanup worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CleanupSubmitError {
    /// Runtime shutdown closed the executor to new work.
    Unavailable,
    /// The bounded queue has no capacity for another distinct job.
    Exhausted,
    /// Executor ownership state was poisoned.
    Poisoned,
}

/// One bounded worker created with the runtime, never on a failure path.
pub(super) struct CleanupExecutor {
    sender: Mutex<Option<SyncSender<CleanupJob>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    worker_stopped: Mutex<Receiver<()>>,
    shutdown_timeout: Duration,
}

impl CleanupExecutor {
    pub(super) fn new(
        queue_capacity: usize,
        shutdown_timeout: Duration,
    ) -> Result<Self, EngineError> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(queue_capacity.max(1));
        let (stopped_sender, stopped_receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name(String::from("aion-process-cleanup"))
            .spawn(move || {
                run_cleanup_worker(&receiver);
                let _ = stopped_sender.send(());
            })
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to provision process cleanup executor: {error}"),
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            worker_stopped: Mutex::new(stopped_receiver),
            shutdown_timeout,
        })
    }

    pub(super) fn submit(&self, job: CleanupJob) -> Result<(), CleanupSubmitError> {
        let sender = self
            .sender
            .lock()
            .map_err(|_| CleanupSubmitError::Poisoned)?;
        let Some(sender) = sender.as_ref() else {
            return Err(CleanupSubmitError::Unavailable);
        };
        sender.try_send(job).map_err(|error| match error {
            TrySendError::Full(_) => CleanupSubmitError::Exhausted,
            TrySendError::Disconnected(_) => CleanupSubmitError::Unavailable,
        })
    }

    pub(super) fn shutdown(&self) -> Result<(), EngineError> {
        let mut sender = lock_executor_state(&self.sender)?;
        sender.take();
        drop(sender);

        if lock_executor_state(&self.worker)?.is_none() {
            return Ok(());
        }
        let stopped =
            lock_executor_state(&self.worker_stopped)?.recv_timeout(self.shutdown_timeout);
        match stopped {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => {
                return Err(EngineError::CleanupExecutorShutdownTimedOut {
                    timeout_millis: self.shutdown_timeout.as_millis(),
                });
            }
        }

        let worker = lock_executor_state(&self.worker)?.take();
        if let Some(worker) = worker {
            worker.join().map_err(|_| EngineError::Runtime {
                reason: String::from("process cleanup executor worker terminated unexpectedly"),
            })?;
        }
        Ok(())
    }
}

fn lock_executor_state<T>(state: &Mutex<T>) -> Result<MutexGuard<'_, T>, EngineError> {
    state
        .lock()
        .map_err(|_| EngineError::CleanupExecutorPoisoned)
}

fn run_cleanup_worker(receiver: &Receiver<CleanupJob>) {
    while let Ok(job) = receiver.recv() {
        job();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn shutdown_is_bounded_and_can_be_retried_after_worker_release()
    -> Result<(), Box<dyn std::error::Error>> {
        let executor = CleanupExecutor::new(1, Duration::from_millis(10))?;
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        executor
            .submit(Box::new(move || {
                let _ = release_receiver.recv();
            }))
            .map_err(|error| format!("blocking cleanup job was refused: {error:?}"))?;

        assert!(matches!(
            executor.shutdown(),
            Err(EngineError::CleanupExecutorShutdownTimedOut { timeout_millis: 10 })
        ));

        release_sender.send(())?;
        executor.shutdown()?;
        Ok(())
    }
}
