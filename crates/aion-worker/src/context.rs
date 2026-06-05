//! `ActivityContext` heartbeat, cancellation, attempt, and identifier support.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aion_core::{ActivityId, Payload};
use tokio::sync::{Notify, mpsc};

use crate::error::WorkerError;

/// Handler-facing context for one activity execution.
#[derive(Clone, Debug)]
pub struct ActivityContext {
    activity_id: ActivityId,
    attempt: u32,
    cancellation: Arc<CancellationState>,
    heartbeat_sender: Option<mpsc::UnboundedSender<HeartbeatRequest>>,
}

/// Internal handle used by the worker runtime to signal cooperative cancellation.
#[derive(Clone, Debug)]
pub struct ActivityCancellationHandle {
    cancellation: Arc<CancellationState>,
}

/// Heartbeat request emitted by [`ActivityContext::heartbeat`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeartbeatRequest {
    /// Activity whose progress is being reported.
    pub activity_id: ActivityId,
    /// Opaque progress detail supplied by the handler.
    pub detail: Option<Payload>,
}

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl ActivityContext {
    /// Creates a context and the internal handle that can signal cancellation.
    #[must_use]
    pub fn new(activity_id: ActivityId, attempt: u32) -> (Self, ActivityCancellationHandle) {
        Self::with_heartbeat_sender(activity_id, attempt, None)
    }

    /// Returns this activity's identifier.
    #[must_use]
    pub const fn activity_id(&self) -> &ActivityId {
        &self.activity_id
    }

    /// Returns this activity's attempt number.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Emits a cooperative heartbeat request for this activity.
    ///
    /// AR-004 wires this seam to the transport session. Until then, contexts
    /// without a heartbeat sender treat heartbeats as a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] when an installed heartbeat seam has been closed.
    pub fn heartbeat(&self, detail: Option<Payload>) -> Result<(), WorkerError> {
        if let Some(sender) = &self.heartbeat_sender {
            sender
                .send(HeartbeatRequest {
                    activity_id: self.activity_id.clone(),
                    detail,
                })
                .map_err(|source| WorkerError::registration(HeartbeatSeamClosed { source }))?;
        }
        Ok(())
    }

    /// Returns true once cooperative cancellation has been signalled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.cancelled.load(Ordering::Acquire)
    }

    /// Resolves when cooperative cancellation is signalled.
    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            self.cancellation.notify.notified().await;
        }
    }

    pub(crate) fn with_heartbeat_sender(
        activity_id: ActivityId,
        attempt: u32,
        heartbeat_sender: Option<mpsc::UnboundedSender<HeartbeatRequest>>,
    ) -> (Self, ActivityCancellationHandle) {
        let cancellation = Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        });
        let context = Self {
            activity_id,
            attempt,
            cancellation: Arc::clone(&cancellation),
            heartbeat_sender,
        };
        let handle = ActivityCancellationHandle { cancellation };
        (context, handle)
    }
}

impl ActivityCancellationHandle {
    /// Signals cooperative cancellation to the handler-facing context.
    pub fn cancel(&self) {
        let was_cancelled = self.cancellation.cancelled.swap(true, Ordering::AcqRel);
        if !was_cancelled {
            self.cancellation.notify.notify_waiters();
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("activity heartbeat seam is closed: {source}")]
struct HeartbeatSeamClosed {
    source: mpsc::error::SendError<HeartbeatRequest>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aion_core::ActivityId;

    use super::ActivityContext;

    #[tokio::test]
    async fn context_exposes_identity_attempt_and_cancellation_signal() {
        let activity_id = ActivityId::from_sequence_position(42);
        let (context, cancellation) = ActivityContext::new(activity_id.clone(), 3);

        assert_eq!(context.activity_id(), &activity_id);
        assert_eq!(context.attempt(), 3);
        assert!(!context.is_cancelled());

        cancellation.cancel();

        assert!(context.is_cancelled());
        let cancelled = tokio::time::timeout(Duration::from_millis(50), context.cancelled()).await;
        assert!(cancelled.is_ok());
    }
}
