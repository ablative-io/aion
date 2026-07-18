//! Constant-cardinality dispatcher for every process-exit callback.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{EngineError, Pid};

use super::{OwnedProcessExitOutcome, ProcessExitCallback, ProcessExitRecord, ProcessExitRecords};

struct ProcessExitCallbackJob {
    callback: ProcessExitCallback,
    terminal: OwnedProcessExitOutcome,
}

pub(super) enum CallbackDispatch {
    Submitted,
    Deferred(ProcessExitCallback),
}

pub(super) struct CallbackDispatchFailure {
    pub(super) callback: ProcessExitCallback,
    pub(super) error: EngineError,
}

pub(super) struct ProcessExitCallbackDispatcher {
    sender: Mutex<Option<SyncSender<ProcessExitCallbackJob>>>,
    queued: Arc<AtomicUsize>,
    #[cfg(test)]
    queue_capacity: usize,
    worker: Mutex<Option<JoinHandle<()>>>,
    stopped: Mutex<Receiver<()>>,
    shutdown_timeout: Duration,
}

impl ProcessExitCallbackDispatcher {
    pub(super) fn new(
        shutdown_timeout: Duration,
        queue_capacity: usize,
        records: Weak<ProcessExitRecords>,
    ) -> Result<Self, EngineError> {
        let queue_capacity = queue_capacity.max(1);
        let (sender, receiver) = std::sync::mpsc::sync_channel(queue_capacity);
        let (stopped_sender, stopped) = std::sync::mpsc::sync_channel(1);
        let queued = Arc::new(AtomicUsize::new(0));
        let worker_queued = Arc::clone(&queued);
        let worker = std::thread::Builder::new()
            .name(String::from("aion-process-exit-callback"))
            .spawn(move || {
                run_worker(&receiver, &records, &worker_queued);
                let _ = stopped_sender.send(());
            })
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to provision process exit callback dispatcher: {error}"),
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            queued,
            #[cfg(test)]
            queue_capacity,
            worker: Mutex::new(Some(worker)),
            stopped: Mutex::new(stopped),
            shutdown_timeout,
        })
    }

    pub(super) fn dispatch(
        &self,
        callback: ProcessExitCallback,
        terminal: OwnedProcessExitOutcome,
    ) -> Result<CallbackDispatch, Box<CallbackDispatchFailure>> {
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
        self.queued.fetch_add(1, Ordering::AcqRel);
        match sender.try_send(job) {
            Ok(()) => Ok(CallbackDispatch::Submitted),
            Err(TrySendError::Full(job)) => {
                self.queued.fetch_sub(1, Ordering::AcqRel);
                Ok(CallbackDispatch::Deferred(job.callback))
            }
            Err(TrySendError::Disconnected(job)) => {
                self.queued.fetch_sub(1, Ordering::AcqRel);
                Err(Box::new(CallbackDispatchFailure {
                    callback: job.callback,
                    error: EngineError::ProcessExitCallbackDispatcherUnavailable,
                }))
            }
        }
    }

    #[cfg(test)]
    pub(super) fn queue_usage(&self) -> (usize, usize) {
        (self.queued.load(Ordering::Acquire), self.queue_capacity)
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

fn run_worker(
    receiver: &Receiver<ProcessExitCallbackJob>,
    records: &Weak<ProcessExitRecords>,
    queued: &AtomicUsize,
) {
    while let Ok(job) = receiver.recv() {
        queued.fetch_sub(1, Ordering::AcqRel);
        invoke_callback(job);
        retry_restored_callbacks(records);
    }
}

fn retry_restored_callbacks(records: &Weak<ProcessExitRecords>) {
    let Some(records) = records.upgrade() else {
        return;
    };
    loop {
        let snapshot: Vec<(Pid, Arc<ProcessExitRecord>)> = records
            .iter()
            .map(|entry| (*entry.key(), Arc::clone(entry.value())))
            .collect();
        let mut found = false;
        for (pid, record) in snapshot {
            match record.take_terminal_callback() {
                Ok(Some((callback, terminal))) => {
                    found = true;
                    invoke_callback(ProcessExitCallbackJob { callback, terminal });
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(pid, %error, "failed to retry a restored process exit callback");
                }
            }
        }
        if !found {
            return;
        }
    }
}

fn invoke_callback(job: ProcessExitCallbackJob) {
    let invoke = move || (job.callback)(job.terminal);
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(invoke)).is_err() {
        tracing::error!("process exit callback terminated unexpectedly");
    }
}
