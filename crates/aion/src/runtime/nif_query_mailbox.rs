use std::sync::Arc;

use aion_core::WorkflowId;

use crate::Pid;
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::query::QueryError;
use crate::registry::Registry;

use super::nif_query::{payload_from_string, registered_handler};

pub(super) struct QueryMailboxEngine {
    registry: Arc<Registry>,
}

impl QueryMailboxEngine {
    pub(super) fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }
}

impl EngineHandle for QueryMailboxEngine {
    fn resolve_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowResidency, EngineSeamError> {
        let handle = self
            .registry
            .list()
            .map_err(|error| delivery_error(error.to_string()))?
            .into_iter()
            .find(|handle| handle.workflow_id() == workflow_id);
        match handle {
            Some(handle) => Ok(WorkflowResidency::Resident(WorkflowProcessHandle::new(
                handle.pid(),
            ))),
            None => Ok(WorkflowResidency::Unknown),
        }
    }

    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError> {
        let WorkflowMailboxMessage::Query { name, reply_to, .. } = message else {
            return Err(delivery_error(
                "query mailbox engine only accepts query messages",
            ));
        };
        let result = match registered_handler(process.pid(), &name) {
            Ok(Some(_handler)) => Ok(payload_from_string("{}")),
            Ok(None) => Err(QueryError::UnknownQuery(name)),
            Err(error) => Err(QueryError::Engine(delivery_error(error))),
        };
        reply_to
            .send(result)
            .map_err(|_| delivery_error("query caller dropped reply receiver"))
    }

    fn spawn_child_workflow(
        &self,
        request: ChildWorkflowSpawnRequest,
    ) -> Result<ChildWorkflowSpawnResult, EngineSeamError> {
        Err(EngineSeamError::ChildSpawn {
            reason: format!(
                "query mailbox engine does not spawn child workflow {}",
                request.workflow_type
            ),
        })
    }

    fn terminate_linked_child_workflow(
        &self,
        parent_workflow_id: &WorkflowId,
        child_process: WorkflowProcessHandle,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::ChildTermination {
            reason: format!(
                "query mailbox engine does not terminate child {child_process:?} for {parent_workflow_id} with correlation {correlation}"
            ),
        })
    }

    fn terminate_linked_activity(
        &self,
        parent_workflow_id: &WorkflowId,
        activity_process: Pid,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::ChildTermination {
            reason: format!(
                "query mailbox engine does not terminate activity {activity_process} for {parent_workflow_id} with correlation {correlation}"
            ),
        })
    }

    fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::TimerWheel {
            reason: format!("query mailbox engine does not arm timer {}", entry.timer_id),
        })
    }

    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &aion_core::TimerId,
    ) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::TimerWheel {
            reason: format!(
                "query mailbox engine does not disarm timer {timer_id} for {process:?}"
            ),
        })
    }

    fn record_workflow_event(
        &self,
        workflow_id: &WorkflowId,
        event: aion_core::Event,
    ) -> Result<(), EngineSeamError> {
        Err(EngineSeamError::Recorder {
            reason: format!(
                "queries must not record event {} for workflow {workflow_id}",
                event.seq()
            ),
        })
    }
}

fn delivery_error(reason: impl Into<String>) -> EngineSeamError {
    EngineSeamError::Delivery {
        reason: reason.into(),
    }
}
