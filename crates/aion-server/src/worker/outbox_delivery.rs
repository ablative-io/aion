//! Server-side outbox completion delivery callback.
//!
//! Bridges an unmatched durable-outbox completion arriving at the worker sink
//! ([`PendingActivities`](super::bridge::PendingActivities)) to the engine: it
//! resolves the workflow to its live process through the engine's active
//! registry and delivers the terminal into that workflow's mailbox, where the
//! engine's `take_and_record` records it. Installed only when the outbox is
//! enabled, so flag-off the unmatched branch stays a silent drop.

use std::sync::Arc;

use aion::Engine;
use aion_core::{ActivityId, RunId, WorkflowId};

use super::bridge::OutboxDeliveryCallback;
use crate::error::ServerError;

/// [`OutboxDeliveryCallback`] backed by the embedded engine.
pub struct ServerOutboxDeliveryCallback {
    engine: Arc<Engine>,
}

impl ServerOutboxDeliveryCallback {
    /// Build a callback over the shared engine handle.
    #[must_use]
    pub fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }
}

impl OutboxDeliveryCallback for ServerOutboxDeliveryCallback {
    fn deliver_completion(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        run_id: Option<&RunId>,
        result: String,
    ) -> Result<bool, ServerError> {
        self.engine.runtime().deliver_outbox_completion(
            self.engine.registry(),
            workflow_id,
            activity_id,
            run_id,
            result,
        )
            .map_err(ServerError::from)
    }

    fn deliver_failure(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        reason: String,
    ) -> Result<bool, ServerError> {
        self.engine
            .runtime()
            .deliver_outbox_failure(self.engine.registry(), workflow_id, activity_id, reason)
            .map_err(ServerError::from)
    }
}
