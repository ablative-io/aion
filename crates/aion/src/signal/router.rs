//! Signal router: record `SignalReceived` and deliver to the mailbox.

use std::sync::Arc;

use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
use aion_store::EventStore;
use chrono::Utc;

use crate::engine_seam::{
    EngineHandle, EngineSeamError, WorkflowMailboxMessage, WorkflowResidency,
};

/// Routes durable signals into resident workflow mailboxes.
///
/// The router consumes AE/AD's engine seam for residency resolution, recorder access, and mailbox
/// delivery. It keeps the configured event store handle as part of the durable interaction service,
/// but does not append to it directly: asynchronous signal observations must go through the target
/// workflow's single recorder seam.
pub struct SignalRouter {
    engine: Arc<dyn EngineHandle>,
    event_store: Arc<dyn EventStore>,
}

impl SignalRouter {
    /// Creates a signal router backed by the engine seam and configured event store.
    #[must_use]
    pub fn new(engine: Arc<dyn EngineHandle>, event_store: Arc<dyn EventStore>) -> Self {
        Self {
            engine,
            event_store,
        }
    }

    /// Returns a clone of the configured event store handle.
    #[must_use]
    pub fn event_store(&self) -> Arc<dyn EventStore> {
        Arc::clone(&self.event_store)
    }

    /// Records a signal observation and then delivers it to a resident workflow mailbox.
    ///
    /// # Errors
    ///
    /// Returns [`SignalRouterError`] when the target workflow is not resident, recorder append fails,
    /// or mailbox delivery fails. Recorder failure returns before delivery, guaranteeing there is no
    /// silent mailbox observation without a durable `SignalReceived` event.
    pub fn signal(
        &self,
        workflow_id: WorkflowId,
        name: impl Into<String>,
        payload: Payload,
    ) -> Result<(), SignalRouterError> {
        let process = match self.engine.resolve_workflow(&workflow_id)? {
            WorkflowResidency::Resident(process) => process,
            WorkflowResidency::NonResident => {
                return Err(SignalRouterError::NonResident {
                    workflow_id: workflow_id.clone(),
                });
            }
            WorkflowResidency::Terminal => {
                return Err(SignalRouterError::Terminal {
                    workflow_id: workflow_id.clone(),
                });
            }
            WorkflowResidency::Unknown => {
                return Err(SignalRouterError::Unknown {
                    workflow_id: workflow_id.clone(),
                });
            }
        };

        let name = name.into();
        let event = Event::SignalReceived {
            envelope: EventEnvelope {
                seq: 0,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            name: name.clone(),
            payload: payload.clone(),
        };

        self.engine
            .record_workflow_event(&workflow_id, event)
            .map_err(SignalRouterError::Record)?;

        self.engine
            .deliver_workflow_message(
                process,
                WorkflowMailboxMessage::SignalReceived { name, payload },
            )
            .map_err(SignalRouterError::Deliver)
    }
}

/// Errors returned by [`SignalRouter`].
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum SignalRouterError {
    /// AE reported the workflow is known but currently not resident.
    #[error("workflow {workflow_id} is not resident")]
    NonResident {
        /// Workflow that had no current live process.
        workflow_id: WorkflowId,
    },

    /// AE reported the workflow is terminal.
    #[error("workflow {workflow_id} is terminal")]
    Terminal {
        /// Terminal workflow identifier.
        workflow_id: WorkflowId,
    },

    /// AE reported no workflow for the requested identifier.
    #[error("workflow {workflow_id} is unknown")]
    Unknown {
        /// Unknown workflow identifier.
        workflow_id: WorkflowId,
    },

    /// The engine seam failed before the route target was known.
    #[error("workflow resolution failed: {0}")]
    Resolve(#[from] EngineSeamError),

    /// Recording `SignalReceived` through the workflow recorder failed.
    #[error("signal recording failed: {0}")]
    Record(EngineSeamError),

    /// Delivering an already-recorded signal to the mailbox failed.
    #[error("signal delivery failed: {0}")]
    Deliver(EngineSeamError),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, Payload, WorkflowId};
    use aion_store::InMemoryStore;

    use super::{SignalRouter, SignalRouterError};
    use crate::engine_seam::test_support::{FakeEngineHandle, FakeEngineOperation};
    use crate::engine_seam::{
        EngineSeamError, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
    };

    #[test]
    fn successful_route_records_signal_before_delivering_message()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = Arc::new(FakeEngineHandle::new());
        let router = SignalRouter::new(engine.clone(), Arc::new(InMemoryStore::default()));
        let workflow_id = WorkflowId::new_v4();
        let process = WorkflowProcessHandle::new(7);
        let payload = payload(b"{\"ok\":true}".to_vec());
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        router.signal(workflow_id.clone(), "wake", payload.clone())?;

        let operations = engine.operations()?;
        assert_eq!(operations.len(), 2);
        if let FakeEngineOperation::EventRecorded {
            workflow_id: recorded_workflow_id,
            event:
                Event::SignalReceived {
                    name,
                    payload: recorded_payload,
                    envelope,
                },
        } = &operations[0]
        {
            assert_eq!(recorded_workflow_id, &workflow_id);
            assert_eq!(envelope.workflow_id, workflow_id);
            assert_eq!(name, "wake");
            assert_eq!(recorded_payload, &payload);
        } else {
            return Err(std::io::Error::other("SignalReceived was not recorded first").into());
        }
        if let FakeEngineOperation::Delivered {
            process: delivered_process,
            message,
        } = &operations[1]
        {
            assert_eq!(*delivered_process, process);
            assert_eq!(
                message,
                &WorkflowMailboxMessage::SignalReceived {
                    name: "wake".to_owned(),
                    payload,
                }
            );
        } else {
            return Err(std::io::Error::other("signal was not delivered second").into());
        }
        Ok(())
    }

    #[test]
    fn record_failure_prevents_delivery() -> Result<(), Box<dyn std::error::Error>> {
        let engine = Arc::new(FakeEngineHandle::new());
        let router = SignalRouter::new(engine.clone(), Arc::new(InMemoryStore::default()));
        let workflow_id = WorkflowId::new_v4();
        engine.set_residency(
            workflow_id.clone(),
            WorkflowResidency::Resident(WorkflowProcessHandle::new(11)),
        )?;
        engine.push_record_response(Err(EngineSeamError::Recorder {
            reason: "append rejected".to_owned(),
        }))?;

        let error = router
            .signal(workflow_id, "wake", payload(b"null".to_vec()))
            .err()
            .ok_or_else(|| std::io::Error::other("record failure was not returned"))?;

        assert!(matches!(error, SignalRouterError::Record(_)));
        assert!(engine.delivered_messages()?.is_empty());
        assert!(engine.operations()?.is_empty());
        Ok(())
    }

    #[test]
    fn delivery_uses_resolved_resident_handle_and_preserves_name_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = Arc::new(FakeEngineHandle::new());
        let router = SignalRouter::new(engine.clone(), Arc::new(InMemoryStore::default()));
        let workflow_id = WorkflowId::new_v4();
        let process = WorkflowProcessHandle::new(99);
        let payload = payload(b"{\"subject\":\"order\"}".to_vec());
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        router.signal(workflow_id, "approved", payload.clone())?;

        assert_eq!(
            engine.delivered_messages()?,
            vec![(
                process,
                WorkflowMailboxMessage::SignalReceived {
                    name: "approved".to_owned(),
                    payload,
                },
            )]
        );
        Ok(())
    }

    fn payload(bytes: Vec<u8>) -> Payload {
        Payload::new(aion_core::ContentType::Json, bytes)
    }
}
