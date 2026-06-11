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
            // Terminal check and signal record are atomic under the recorder
            // lock: the exit monitor records terminal events through the same
            // recorder, and a terminal run must reject signals instead of
            // appending after its terminal event or deferring to a resume
            // queue that can never drain.
            let history = recorder.read_history().await.map_err(EngineError::from)?;
            if crate::engine::delegated::run_has_terminal_history(&history, target.run_id()) {
                return Err(SignalRouterError::Terminal {
                    workflow_id: target.workflow_id().clone(),
                    run_id: target.run_id().clone(),
                }
                .into());
            }
            recorder
                .record_signal_received(Utc::now(), name.clone(), payload.clone())
                .await?;
        }

        match target.residency() {
            HandleResidency::Resident => {
                let Err(error) = self
                    .runtime
                    .deliver_signal_received_async(target.pid())
                    .await
                else {
                    return Ok(());
                };
                // The signal is already durable; the marker is only a wake.
                // A process that exited between the record and this delivery
                // (completion racing the signal — including a surplus wake
                // letting the workflow resolve the just-recorded signal from
                // history and return before the marker lands) has accepted
                // the signal: its terminal is recorded by the exit monitor,
                // and a crashed run replays with the signal in history. Only
                // a *live* process the marker cannot reach is a delivery
                // failure.
                if !self.runtime.is_live(target.pid()) {
                    tracing::debug!(
                        workflow_id = %target.workflow_id(),
                        run_id = %target.run_id(),
                        signal_name = %name,
                        process = target.pid(),
                        "recorded signal raced process exit; durable record stands"
                    );
                    return Ok(());
                }
                let reason = error.to_string();
                tracing::warn!(
                    workflow_id = %target.workflow_id(),
                    run_id = %target.run_id(),
                    signal_name = %name,
                    process = target.pid(),
                    error = %reason,
                    "durably recorded signal could not be delivered to resident workflow mailbox"
                );
                Err(EngineError::from(SignalRouterError::DeliveryFailed {
                    workflow_id: target.workflow_id().clone(),
                    run_id: target.run_id().clone(),
                    process_id: target.pid(),
                    signal_name: name,
                    reason,
                }))
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, Payload, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::ConcreteSignalRouter;
    use crate::durability::Recorder;
    use crate::engine::delegated::SignalRouter;
    use crate::registry::{
        CompletionNotifier, HandleResidency, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::{RuntimeConfig, RuntimeHandle};
    use crate::signal::SignalResumeHandoff;
    use crate::{EngineError, SignalRouterError};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    async fn started_workflow_handle(
        store: &Arc<dyn EventStore>,
        pid: u64,
    ) -> Result<WorkflowHandle, Box<dyn std::error::Error>> {
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(store));
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                crate::durability::WorkflowStartRecord {
                    workflow_type: "checkout".to_owned(),
                    input: payload("input")?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;
        Ok(WorkflowHandle::new(WorkflowHandleParts {
            workflow_id,
            run_id,
            pid,
            workflow_type: "checkout".to_owned(),
            loaded_version: ContentHash::from_bytes([3; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        }))
    }

    fn router() -> Result<ConcreteSignalRouter, Box<dyn std::error::Error>> {
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        Ok(ConcreteSignalRouter::new(
            runtime,
            Arc::new(SignalResumeHandoff::new()),
        ))
    }

    /// A resident process that exited between the durable record and the
    /// marker delivery has accepted the signal: the record is the contract,
    /// the marker is only a wake. Before the fix this returned
    /// `DeliveryFailed` for an already-accepted signal, which surfaced as a
    /// flaky engine error whenever a completion (or a surplus wake letting
    /// the workflow resolve the just-recorded signal from history) raced the
    /// delivery.
    #[tokio::test]
    async fn dead_resident_pid_records_signal_and_resolves_as_accepted() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        // No process with this pid exists in the runtime: this is the
        // window where a resident process exited but the exit monitor has
        // not reconciled the registry yet.
        let handle = started_workflow_handle(&store, 424_242).await?;
        let sent = payload("recorded")?;

        router()?
            .route(&handle, "wake".to_owned(), sent.clone())
            .await?;

        let history = store.read_history(handle.workflow_id()).await?;
        let recorded = history
            .iter()
            .find_map(|event| match event {
                Event::SignalReceived { name, payload, .. } => Some((name, payload)),
                _ => None,
            })
            .ok_or("SignalReceived was not recorded")?;
        assert_eq!(recorded.0, "wake");
        assert_eq!(recorded.1, &sent);
        Ok(())
    }

    #[tokio::test]
    async fn terminal_run_rejects_signal_without_appending() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let handle = started_workflow_handle(&store, 424_243).await?;
        {
            let recorder = handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_failed(
                    chrono::Utc::now(),
                    aion_core::WorkflowError {
                        message: "killed".to_owned(),
                        details: None,
                    },
                )
                .await?;
        }
        let terminal_len = store.read_history(handle.workflow_id()).await?.len();

        let error = router()?
            .route(&handle, "wake".to_owned(), payload("rejected")?)
            .await
            .err()
            .ok_or("signal to terminal run unexpectedly succeeded")?;

        assert!(matches!(
            error,
            EngineError::SignalRouter(SignalRouterError::Terminal { .. })
        ));
        assert_eq!(
            store.read_history(handle.workflow_id()).await?.len(),
            terminal_len
        );
        Ok(())
    }
}
