//! Concrete delegated signal router: record `SignalReceived`, then deliver to the mailbox.

use std::sync::Arc;

use aion_core::Payload;
use async_trait::async_trait;
use chrono::Utc;

use crate::{
    EngineError, HandleResidency, RuntimeHandle, SignalRouterError, WorkflowHandle,
    engine::delegated, signal::SignalResumeHandoff,
};

/// Delegated signal router for resident workflow processes.
///
/// Signals are first recorded through the target handle's single-writer recorder.
/// Only after that durable append succeeds does the router enqueue the signal
/// marker into the target runtime mailbox, preserving record-before-deliver
/// crash-safety.
#[derive(Clone)]
pub struct ConcreteSignalRouter {
    runtime: Arc<RuntimeHandle>,
    handoff: Arc<SignalResumeHandoff>,
}

impl ConcreteSignalRouter {
    /// Create a router that delivers recorded signals through `runtime` and defers through `handoff`.
    #[must_use]
    pub fn new(runtime: Arc<RuntimeHandle>, handoff: Arc<SignalResumeHandoff>) -> Self {
        Self { runtime, handoff }
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

        match target.residency() {
            HandleResidency::Resident => {
                self.runtime
                    .deliver_signal_received(target.pid(), name, payload)
            }
            HandleResidency::Suspended => self
                .handoff
                .defer(target.workflow_id().clone(), name, payload)
                .map_err(|error| {
                    EngineError::from(SignalRouterError::Handoff {
                        reason: error.to_string(),
                    })
                }),
        }
    }
}
