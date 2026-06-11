//! Mailbox-delivery engine handle for live workflow queries.
//!
//! Delivery never executes the handler itself: it queues a pending-query
//! record for the target pid, parks the reply sender keyed by query id, and
//! enqueues an `aion_query` wake marker. The workflow's next suspending-await
//! invocation drains the queue through the query pump and replies over the
//! parked sender — nothing on this path touches the recorder or resolver.

use std::sync::{Arc, Weak};

use aion_core::{WorkflowId, WorkflowStatus};
use uuid::Uuid;

use crate::Pid;
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::query::QueryError;
use crate::registry::{HandleResidency, Registry};
use crate::runtime::RuntimeHandle;

use super::nif_query::{
    insert_pending_reply, is_query_registered, prune_closed_pending_replies, take_pending_reply,
};
use super::nif_state::{EngineNifState, PendingQuery};

pub(super) struct QueryMailboxEngine {
    registry: Arc<Registry>,
    // Weak: the engine state owns this engine through its query bridge slot.
    nif_state: Weak<EngineNifState>,
    // Weak: the runtime owns the engine state that owns this engine.
    runtime: Weak<RuntimeHandle>,
}

impl QueryMailboxEngine {
    pub(super) fn new(
        registry: Arc<Registry>,
        nif_state: Weak<EngineNifState>,
        runtime: Weak<RuntimeHandle>,
    ) -> Self {
        Self {
            registry,
            nif_state,
            runtime,
        }
    }

    /// Park the reply, queue the query, and wake the workflow process.
    ///
    /// On marker-delivery failure every inserted entry is removed before the
    /// typed error is reported, so a failed delivery leaves no stale state.
    fn enqueue_query(
        &self,
        state: &EngineNifState,
        pid: u64,
        name: String,
        reply_to: crate::engine_seam::QueryReplySender,
    ) -> Result<(), QueryError> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| QueryError::Engine(delivery_error("engine runtime has shut down")))?;
        // Hygiene: drop senders whose caller already timed out, so a
        // never-woken workflow does not accumulate stale reply channels.
        prune_closed_pending_replies(state)
            .map_err(|error| QueryError::Engine(delivery_error(error)))?;
        let query_id = Uuid::new_v4().to_string();
        insert_pending_reply(state, query_id.clone(), pid, reply_to)
            .map_err(|error| QueryError::Engine(delivery_error(error)))?;
        state
            .pending_queries
            .entry(pid)
            .or_default()
            .push_back(PendingQuery {
                query_id: query_id.clone(),
                name,
            });
        if let Err(error) = runtime.deliver_query_request(pid) {
            // Roll back both entries; the caller gets the typed failure
            // through its own reply channel.
            if let Some(mut queue) = state.pending_queries.get_mut(&pid) {
                queue.retain(|pending| pending.query_id != query_id);
            }
            let removed = take_pending_reply(state, &query_id)
                .map_err(|reason| QueryError::Engine(delivery_error(reason)))?;
            drop(removed);
            return Err(QueryError::Engine(delivery_error(format!(
                "query wake marker delivery failed: {error}"
            ))));
        }
        Ok(())
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
            Some(handle) if handle.cached_status() != WorkflowStatus::Running => {
                Ok(WorkflowResidency::Terminal)
            }
            Some(handle) => match handle.residency() {
                HandleResidency::Resident => Ok(WorkflowResidency::Resident(
                    WorkflowProcessHandle::new(handle.pid()),
                )),
                // A suspended workflow has no live process to answer from;
                // AT-007 forbids resuming solely to answer a query.
                HandleResidency::Suspended => Ok(WorkflowResidency::NonResident),
            },
            None => Ok(WorkflowResidency::Unknown),
        }
    }

    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError> {
        let WorkflowMailboxMessage::Query {
            name,
            reply_to,
            // The wire carries no query arguments yet; the payload is always
            // the empty JSON object and is not forwarded to the handler.
            payload: _,
        } = message
        else {
            return Err(delivery_error(
                "query mailbox engine only accepts query messages",
            ));
        };
        let Some(state) = self.nif_state.upgrade() else {
            return reply_to
                .send(Err(QueryError::Engine(delivery_error(
                    "engine NIF state has been dropped",
                ))))
                .map_err(|_| delivery_error("query caller dropped reply receiver"));
        };
        match is_query_registered(&state, process.pid(), &name) {
            Ok(true) => match self.enqueue_query(&state, process.pid(), name, reply_to) {
                Ok(()) => Ok(()),
                // The reply sender was consumed by the rollback inside
                // `enqueue_query`; surface the failure to the seam caller.
                Err(error) => Err(delivery_error(format!("query enqueue failed: {error}"))),
            },
            // An unregistered name never disturbs the workflow process.
            Ok(false) => reply_to
                .send(Err(QueryError::UnknownQuery(name)))
                .map_err(|_| delivery_error("query caller dropped reply receiver")),
            Err(error) => reply_to
                .send(Err(QueryError::Engine(delivery_error(error))))
                .map_err(|_| delivery_error("query caller dropped reply receiver")),
        }
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
