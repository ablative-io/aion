//! Concrete delegated signal router: record `SignalReceived`, then deliver to the mailbox.

use std::sync::Arc;

use aion_core::Payload;
use async_trait::async_trait;
use chrono::Utc;

use crate::{EngineError, RuntimeHandle, WorkflowHandle, engine::delegated};

/// Delegated signal router for resident workflow processes.
///
/// Signals are first recorded through the target handle's single-writer recorder.
/// Only after that durable append succeeds does the router enqueue the signal
/// marker into the target runtime mailbox, preserving record-before-deliver
/// crash-safety.
#[derive(Clone)]
pub struct ConcreteSignalRouter {
    runtime: Arc<RuntimeHandle>,
}

impl ConcreteSignalRouter {
    /// Create a router that delivers recorded signals through `runtime`.
    #[must_use]
    pub fn new(runtime: Arc<RuntimeHandle>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl delegated::SignalRouter for ConcreteSignalRouter {
    async fn route(
        &self,
        target: &WorkflowHandle,
        name: String,
        payload: Payload,
    ) -> Result<(), EngineError> {
        let recorder = target.recorder();
        {
            let mut recorder = recorder.lock().await;
            recorder
                .record_signal_received(Utc::now(), name.clone(), payload.clone())
                .await?;
        }

        self.runtime
            .deliver_signal_received(target.pid(), name, payload)
    }
}
