//! non-resident delivery + resume handoff via the engine handle

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use aion_core::{Payload, WorkflowId};

use crate::engine_seam::{
    EngineHandle, EngineSeamError, WorkflowMailboxMessage, WorkflowResidency,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct DeferredSignal {
    name: String,
    payload: Payload,
}

/// In-memory live handoff queue for already-recorded non-resident signals.
///
/// This registry is engine-runtime state only. Durability is provided by the `SignalReceived` event
/// that was already recorded through the workflow's single recorder before a signal is deferred.
/// Within one engine lifetime this queue prevents duplicate live mailbox delivery across repeated
/// resume handoffs. Across a full restart the queue is intentionally empty; AD replay, not this
/// live handoff path, returns recorded signals from workflow history when execution re-reaches the
/// receive point.
#[derive(Default)]
pub struct SignalResumeHandoff {
    deferred: Mutex<HashMap<WorkflowId, VecDeque<DeferredSignal>>>,
}

impl SignalResumeHandoff {
    /// Creates an empty deferred-signal handoff registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an already-recorded signal for live mailbox delivery when the workflow resumes.
    ///
    /// # Errors
    ///
    /// Returns [`SignalResumeError::State`] if the in-memory registry lock is poisoned.
    pub fn defer(
        &self,
        workflow_id: WorkflowId,
        name: String,
        payload: Payload,
    ) -> Result<(), SignalResumeError> {
        self.state()?
            .entry(workflow_id)
            .or_default()
            .push_back(DeferredSignal { name, payload });
        Ok(())
    }

    /// Delivers queued signals for a workflow that AE has made resident.
    ///
    /// Signals are delivered in FIFO order, matching the order in which the router recorded and
    /// deferred them. A delivered signal is removed immediately, so repeated handoff calls in the
    /// same engine lifetime do not redeliver it. If delivery fails, the failed signal and all later
    /// signals remain queued for a later retry.
    ///
    /// # Errors
    ///
    /// Returns [`SignalResumeError`] when residency resolution fails, the workflow is not resident,
    /// mailbox delivery fails, or the in-memory registry lock is poisoned.
    pub fn deliver_deferred(
        &self,
        engine: &dyn EngineHandle,
        workflow_id: &WorkflowId,
    ) -> Result<usize, SignalResumeError> {
        let process = match engine
            .resolve_workflow(workflow_id)
            .map_err(SignalResumeError::Resolve)?
        {
            WorkflowResidency::Resident(process) => process,
            WorkflowResidency::NonResident => {
                return Err(SignalResumeError::NonResident {
                    workflow_id: workflow_id.clone(),
                });
            }
            WorkflowResidency::Terminal => {
                return Err(SignalResumeError::Terminal {
                    workflow_id: workflow_id.clone(),
                });
            }
            WorkflowResidency::Unknown => {
                return Err(SignalResumeError::Unknown {
                    workflow_id: workflow_id.clone(),
                });
            }
        };

        let mut delivered = 0;
        let mut state = self.state()?;
        let Some(queue) = state.get_mut(workflow_id) else {
            return Ok(0);
        };

        while let Some(signal) = queue.front().cloned() {
            engine
                .deliver_workflow_message(
                    process,
                    WorkflowMailboxMessage::SignalReceived {
                        name: signal.name,
                        payload: signal.payload,
                    },
                )
                .map_err(SignalResumeError::Deliver)?;
            queue.pop_front();
            delivered += 1;
        }

        state.remove(workflow_id);
        Ok(delivered)
    }

    /// Returns the number of deferred signals currently queued for the workflow.
    ///
    /// # Errors
    ///
    /// Returns [`SignalResumeError::State`] if the in-memory registry lock is poisoned.
    pub fn pending_count(&self, workflow_id: &WorkflowId) -> Result<usize, SignalResumeError> {
        Ok(self.state()?.get(workflow_id).map_or(0, VecDeque::len))
    }

    fn state(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<WorkflowId, VecDeque<DeferredSignal>>>, SignalResumeError>
    {
        self.deferred.lock().map_err(|_| SignalResumeError::State {
            reason: "deferred signal registry lock was poisoned".to_owned(),
        })
    }
}

/// Errors returned by [`SignalResumeHandoff`].
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum SignalResumeError {
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

    /// The engine seam failed before the resume target was known.
    #[error("workflow resolution failed: {0}")]
    Resolve(EngineSeamError),

    /// Delivering a deferred, already-recorded signal to the mailbox failed.
    #[error("deferred signal delivery failed: {0}")]
    Deliver(EngineSeamError),

    /// The in-memory deferred-signal registry failed.
    #[error("deferred signal registry failed: {reason}")]
    State {
        /// Human-readable registry failure reason.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{ContentType, Payload, WorkflowId};

    use super::SignalResumeHandoff;
    use crate::engine_seam::test_support::{DeliveredWorkflowMessage, FakeEngineHandle};
    use crate::engine_seam::{WorkflowProcessHandle, WorkflowResidency};

    #[test]
    fn deferred_signals_deliver_in_order_exactly_once() -> Result<(), Box<dyn std::error::Error>> {
        let engine = Arc::new(FakeEngineHandle::new());
        let handoff = SignalResumeHandoff::new();
        let workflow_id = WorkflowId::new_v4();
        let process = WorkflowProcessHandle::new(41);
        let first = payload(b"{\"n\":1}".to_vec());
        let second = payload(b"{\"n\":2}".to_vec());

        handoff.defer(workflow_id.clone(), "first".to_owned(), first.clone())?;
        handoff.defer(workflow_id.clone(), "second".to_owned(), second.clone())?;
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        assert_eq!(handoff.deliver_deferred(engine.as_ref(), &workflow_id)?, 2);
        assert_eq!(handoff.deliver_deferred(engine.as_ref(), &workflow_id)?, 0);

        assert_eq!(
            engine.delivered_messages()?,
            vec![
                (
                    process,
                    DeliveredWorkflowMessage::SignalReceived {
                        name: "first".to_owned(),
                        payload: first,
                    },
                ),
                (
                    process,
                    DeliveredWorkflowMessage::SignalReceived {
                        name: "second".to_owned(),
                        payload: second,
                    },
                ),
            ]
        );
        assert_eq!(handoff.pending_count(&workflow_id)?, 0);
        Ok(())
    }

    fn payload(bytes: Vec<u8>) -> Payload {
        Payload::new(ContentType::Json, bytes)
    }
}
