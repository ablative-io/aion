//! Runtime-provisioned executor for process abort and cleanup jobs.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;

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
}

impl CleanupExecutor {
    pub(super) fn new(queue_capacity: usize) -> Result<Self, EngineError> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(queue_capacity.max(1));
        let worker = std::thread::Builder::new()
            .name(String::from("aion-process-cleanup"))
            .spawn(move || run_cleanup_worker(&receiver))
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to provision process cleanup executor: {error}"),
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
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
