//! Concrete delegated query service: residency and terminal guards, then
//! non-recording mailbox dispatch through the query mailbox engine.

use std::sync::Arc;
use std::time::Duration;

use aion_core::{ContentType, Payload};
use async_trait::async_trait;

use crate::engine::delegated;
use crate::engine_seam::{EngineHandle, WorkflowProcessHandle};
use crate::registry::HandleResidency;
use crate::{EngineError, WorkflowHandle};

use super::service::{QueryError, QueryService};

/// Delegated query service for resident workflow processes.
///
/// The engine resolves the live `(workflow, run)` handle; this service
/// rejects suspended residency (AT-007: never resume a workflow solely to
/// answer a query) and terminal runs, then dispatches run-exact through the
/// AT [`QueryService`] over the engine's query mailbox seam. Nothing on this
/// path records events.
pub struct ConcreteQueryService {
    mailbox_engine: Arc<dyn EngineHandle>,
    query_timeout: Duration,
}

impl ConcreteQueryService {
    /// Create a query service over the engine's query mailbox seam with the
    /// engine-configured reply timeout.
    #[must_use]
    pub fn new(mailbox_engine: Arc<dyn EngineHandle>, query_timeout: Duration) -> Self {
        Self {
            mailbox_engine,
            query_timeout,
        }
    }
}

#[async_trait]
impl delegated::QueryService for ConcreteQueryService {
    async fn query(&self, target: &WorkflowHandle, name: String) -> Result<Payload, EngineError> {
        if target.residency() == HandleResidency::Suspended {
            // A suspended workflow has no live heap to answer from, and
            // resuming solely to answer is forbidden.
            return Err(QueryError::NotRunning(target.workflow_id().clone()).into());
        }
        {
            // Terminal check under the recorder lock: the exit monitor
            // records terminal events through this same recorder, so a run
            // observed non-terminal here was non-terminal when the check
            // ran — the remaining completion race surfaces as a typed
            // ReplyDropped, never a hang.
            let recorder = target.recorder();
            let recorder = recorder.lock().await;
            let history = recorder.read_history().await.map_err(EngineError::from)?;
            if crate::engine::delegated::run_has_terminal_history(&history, target.run_id()) {
                return Err(QueryError::NotRunning(target.workflow_id().clone()).into());
            }
        }
        let service = QueryService::new(Arc::clone(&self.mailbox_engine), self.query_timeout);
        service
            .query_process(
                WorkflowProcessHandle::new(target.pid()),
                name,
                // The wire carries no query arguments yet (CLIENT-CONTRACT:
                // args are future work); handlers receive no payload.
                Payload::new(ContentType::Json, b"{}".to_vec()),
            )
            .await
            .map_err(EngineError::Query)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::Duration;

    use aion_core::{ContentType, Event, Payload, TimerId, WorkflowId, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore};

    use super::ConcreteQueryService;
    use crate::EngineError;
    use crate::Pid;
    use crate::durability::Recorder;
    use crate::engine::delegated::QueryService as _;
    use crate::engine_seam::{
        ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
        TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
    };
    use crate::query::QueryError;
    use crate::registry::{
        CompletionNotifier, HandleResidency, WorkflowHandle, WorkflowHandleParts,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const QUERY_TIMEOUT: Duration = Duration::from_millis(50);

    /// Replying fake over the mailbox seam, counting deliveries per process.
    #[derive(Default)]
    struct ReplyingMailbox {
        replies: Mutex<HashMap<String, Payload>>,
        delivered: Mutex<Vec<(u64, String)>>,
    }

    impl ReplyingMailbox {
        fn with_reply(name: &str, payload: Payload) -> Self {
            let fake = Self::default();
            match fake.replies.lock() {
                Ok(mut replies) => {
                    replies.insert(name.to_owned(), payload);
                }
                Err(_) => unreachable!("fresh mutex cannot be poisoned"),
            }
            fake
        }

        fn delivered(&self) -> Result<Vec<(u64, String)>, EngineSeamError> {
            Ok(self.lock_delivered()?.clone())
        }

        fn lock_delivered(&self) -> Result<MutexGuard<'_, Vec<(u64, String)>>, EngineSeamError> {
            self.delivered
                .lock()
                .map_err(|_| EngineSeamError::Delivery {
                    reason: "fake delivered lock was poisoned".to_owned(),
                })
        }
    }

    impl EngineHandle for ReplyingMailbox {
        fn resolve_workflow(
            &self,
            _workflow_id: &WorkflowId,
        ) -> Result<WorkflowResidency, EngineSeamError> {
            Err(EngineSeamError::Delivery {
                reason: "ConcreteQueryService must dispatch run-exact, never resolve".to_owned(),
            })
        }

        fn deliver_workflow_message(
            &self,
            process: WorkflowProcessHandle,
            message: WorkflowMailboxMessage,
        ) -> Result<(), EngineSeamError> {
            let WorkflowMailboxMessage::Query { name, reply_to, .. } = message else {
                return Err(EngineSeamError::Delivery {
                    reason: "fake mailbox only accepts query messages".to_owned(),
                });
            };
            self.lock_delivered()?.push((process.pid(), name.clone()));
            let reply = self
                .replies
                .lock()
                .map_err(|_| EngineSeamError::Delivery {
                    reason: "fake replies lock was poisoned".to_owned(),
                })?
                .get(&name)
                .cloned();
            let result = reply.ok_or(QueryError::UnknownQuery(name));
            reply_to
                .send(result)
                .map_err(|_| EngineSeamError::Delivery {
                    reason: "query caller dropped reply receiver".to_owned(),
                })
        }

        fn spawn_child_workflow(
            &self,
            request: ChildWorkflowSpawnRequest,
        ) -> Result<ChildWorkflowSpawnResult, EngineSeamError> {
            Err(EngineSeamError::ChildSpawn {
                reason: request.workflow_type,
            })
        }

        fn terminate_linked_child_workflow(
            &self,
            parent_workflow_id: &WorkflowId,
            child_process: WorkflowProcessHandle,
            correlation: u64,
        ) -> Result<(), EngineSeamError> {
            Err(EngineSeamError::ChildTermination {
                reason: format!("{parent_workflow_id}:{child_process:?}:{correlation}"),
            })
        }

        fn terminate_linked_activity(
            &self,
            parent_workflow_id: &WorkflowId,
            activity_process: Pid,
            correlation: u64,
        ) -> Result<(), EngineSeamError> {
            Err(EngineSeamError::ChildTermination {
                reason: format!("{parent_workflow_id}:{activity_process}:{correlation}"),
            })
        }

        fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError> {
            Err(EngineSeamError::TimerWheel {
                reason: entry.timer_id.to_string(),
            })
        }

        fn disarm_timer(
            &self,
            process: WorkflowProcessHandle,
            timer_id: &TimerId,
        ) -> Result<(), EngineSeamError> {
            Err(EngineSeamError::TimerWheel {
                reason: format!("{process:?}:{timer_id}"),
            })
        }

        fn record_workflow_event(
            &self,
            workflow_id: &WorkflowId,
            event: Event,
        ) -> Result<crate::engine_seam::RecordOutcome, EngineSeamError> {
            Err(EngineSeamError::Recorder {
                reason: format!(
                    "queries must not record event {} for {workflow_id}",
                    event.seq()
                ),
            })
        }
    }

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&serde_json::json!({ "label": label }))
    }

    async fn started_handle(
        store: &Arc<dyn EventStore>,
        pid: u64,
        residency: HandleResidency,
    ) -> Result<WorkflowHandle, Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
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
            namespace: String::from("default"),
            loaded_version: ContentHash::from_bytes([5; 32]),
            cached_status: WorkflowStatus::Running,
            residency,
            recorder,
            completion: CompletionNotifier::new(),
        }))
    }

    fn assert_not_running(
        result: Result<Payload, EngineError>,
        handle: &WorkflowHandle,
    ) -> Result<(), String> {
        match result {
            Err(EngineError::Query(QueryError::NotRunning(workflow_id)))
                if &workflow_id == handle.workflow_id() =>
            {
                Ok(())
            }
            other => Err(format!(
                "expected NotRunning for {}, got {other:?}",
                handle.workflow_id()
            )),
        }
    }

    #[tokio::test]
    async fn happy_path_dispatches_run_exact_and_returns_handler_reply() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let handle = started_handle(&store, 31, HandleResidency::Resident).await?;
        let reply = Payload::new(ContentType::Json, b"{\"n\":1}".to_vec());
        let mailbox = Arc::new(ReplyingMailbox::with_reply("state", reply.clone()));
        let service = ConcreteQueryService::new(Arc::clone(&mailbox) as _, QUERY_TIMEOUT);

        let returned = service.query(&handle, "state".to_owned()).await?;

        assert_eq!(returned, reply);
        assert_eq!(mailbox.delivered()?, vec![(31, "state".to_owned())]);
        Ok(())
    }

    #[tokio::test]
    async fn suspended_residency_is_not_running_and_never_delivers() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let handle = started_handle(&store, 32, HandleResidency::Suspended).await?;
        let mailbox = Arc::new(ReplyingMailbox::with_reply("state", payload("never-used")?));
        let service = ConcreteQueryService::new(Arc::clone(&mailbox) as _, QUERY_TIMEOUT);

        let result = service.query(&handle, "state".to_owned()).await;

        assert_not_running(result, &handle)?;
        assert!(
            mailbox.delivered()?.is_empty(),
            "a suspended workflow must never be resumed or disturbed to answer a query"
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_history_is_not_running_and_never_delivers() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let handle = started_handle(&store, 33, HandleResidency::Resident).await?;
        {
            let recorder = handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_workflow_completed(chrono::Utc::now(), payload("done")?)
                .await?;
        }
        let mailbox = Arc::new(ReplyingMailbox::with_reply("state", payload("never-used")?));
        let service = ConcreteQueryService::new(Arc::clone(&mailbox) as _, QUERY_TIMEOUT);

        let result = service.query(&handle, "state".to_owned()).await;

        assert_not_running(result, &handle)?;
        assert!(mailbox.delivered()?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn unknown_query_propagates_typed_through_engine_error() -> TestResult {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let handle = started_handle(&store, 34, HandleResidency::Resident).await?;
        let mailbox = Arc::new(ReplyingMailbox::default());
        let service = ConcreteQueryService::new(Arc::clone(&mailbox) as _, QUERY_TIMEOUT);

        let result = service.query(&handle, "missing".to_owned()).await;

        match result {
            Err(EngineError::Query(QueryError::UnknownQuery(name))) => {
                assert_eq!(name, "missing");
                Ok(())
            }
            other => Err(format!("expected UnknownQuery, got {other:?}").into()),
        }
    }
}
