//! `EngineHandle` seam implementation for [`Engine`]: workflow residency
//! resolution and mailbox delivery, with the remaining seam operations
//! routed through dedicated bridges rather than this handle.

use aion_core::{Event, WorkflowId};

use crate::engine_seam::{
    EngineHandle, EngineSeamError, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};

use super::api::Engine;

impl EngineHandle for Engine {
    fn resolve_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowResidency, EngineSeamError> {
        let handle = self
            .registry()
            .list()
            .map_err(|error| EngineSeamError::Delivery {
                reason: error.to_string(),
            })?
            .into_iter()
            .find(|handle| handle.workflow_id() == workflow_id);
        match handle {
            Some(handle) if handle.residency() == crate::HandleResidency::Resident => Ok(
                WorkflowResidency::Resident(WorkflowProcessHandle::new(handle.pid())),
            ),
            Some(_) => Ok(WorkflowResidency::NonResident),
            None => Ok(WorkflowResidency::Unknown),
        }
    }

    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError> {
        match message {
            WorkflowMailboxMessage::SignalReceived { .. } => self
                .runtime()
                .deliver_signal_received(process.pid())
                .map_err(|error| EngineSeamError::Delivery {
                    reason: error.to_string(),
                }),
            other => Err(EngineSeamError::Delivery {
                reason: format!("unsupported workflow mailbox message: {other:?}"),
            }),
        }
    }

    fn spawn_child_workflow(
        &self,
        request: crate::engine_seam::ChildWorkflowSpawnRequest,
    ) -> Result<crate::engine_seam::ChildWorkflowSpawnResult, EngineSeamError> {
        let _ = request;
        Err(EngineSeamError::ChildSpawn {
            reason: "engine handle child spawning is not wired here".to_owned(),
        })
    }

    fn terminate_linked_child_workflow(
        &self,
        parent_workflow_id: &WorkflowId,
        child_process: WorkflowProcessHandle,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        let _ = (parent_workflow_id, child_process, correlation);
        Err(EngineSeamError::ChildTermination {
            reason: "engine handle child termination is not wired here".to_owned(),
        })
    }

    fn terminate_linked_activity(
        &self,
        parent_workflow_id: &WorkflowId,
        activity_process: crate::Pid,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        let _ = (parent_workflow_id, activity_process, correlation);
        Err(EngineSeamError::ChildTermination {
            reason: "engine handle activity termination is not wired here".to_owned(),
        })
    }

    fn arm_timer(&self, entry: crate::engine_seam::TimerWheelEntry) -> Result<(), EngineSeamError> {
        let _ = entry;
        Err(EngineSeamError::TimerWheel {
            reason: "engine handle timer arming is not wired here".to_owned(),
        })
    }

    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &aion_core::TimerId,
    ) -> Result<(), EngineSeamError> {
        let _ = (process, timer_id);
        Err(EngineSeamError::TimerWheel {
            reason: "engine handle timer disarming is not wired here".to_owned(),
        })
    }

    fn record_workflow_event(
        &self,
        workflow_id: &WorkflowId,
        event: Event,
    ) -> Result<(), EngineSeamError> {
        let _ = (workflow_id, event);
        Err(EngineSeamError::Recorder {
            reason: "engine handle event recording is not wired here".to_owned(),
        })
    }
}
