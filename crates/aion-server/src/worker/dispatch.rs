//! Push dispatch for remote activity workers and result handoff to the engine contract.

use aion_core::{ActivityError, ActivityId, Payload, WorkflowId};
use aion_proto::{
    ProtoActivityId, ProtoActivityResult, ProtoActivityTask, ProtoPayload, ProtoWorkflowId,
    WireError, proto_activity_result,
};

use crate::error::ServerError;
use crate::worker::registry::{ConnectedWorkerRegistry, WorkerHandle};

/// Scheduled remote activity that must be placed with a connected worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledActivity {
    /// Namespace selected by the adapter boundary before dispatch.
    pub namespace: String,
    /// Activity type to match against worker registrations.
    pub activity_type: String,
    /// Owning workflow id.
    pub workflow_id: WorkflowId,
    /// Correlating activity id.
    pub activity_id: ActivityId,
    /// Opaque activity input payload.
    pub input: Payload,
}

impl ScheduledActivity {
    /// Build the wire task pushed to the worker stream.
    #[must_use]
    pub fn to_task(&self) -> ProtoActivityTask {
        ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(self.workflow_id.clone())),
            activity_id: Some(ProtoActivityId::from(self.activity_id.clone())),
            activity_type: self.activity_type.clone(),
            input: Some(ProtoPayload::from(self.input.clone())),
        }
    }
}

/// Push dispatcher backed by the connected-worker registry.
#[derive(Clone, Debug)]
pub struct ActivityDispatcher {
    registry: ConnectedWorkerRegistry,
}

impl ActivityDispatcher {
    /// Build a dispatcher over the shared worker registry.
    #[must_use]
    pub fn new(registry: ConnectedWorkerRegistry) -> Self {
        Self { registry }
    }

    /// Push a scheduled activity to a matching worker.
    ///
    /// # Errors
    ///
    /// Returns a typed dispatch error if no worker is available or the selected
    /// stream is closed; returns lock poison if registry access cannot be trusted.
    pub async fn dispatch(&self, activity: &ScheduledActivity) -> Result<(), ServerError> {
        let mut workers = self
            .registry
            .workers_for(&activity.namespace, &activity.activity_type)?;
        workers.sort_by_key(WorkerHandle::id);

        if workers.is_empty() {
            return Err(ServerError::worker_dispatch(
                activity.namespace.clone(),
                activity.activity_type.clone(),
                "no connected worker for activity type",
            ));
        }

        for worker in workers {
            if worker.sender().send(activity.to_task()).await.is_ok() {
                return Ok(());
            }
            self.registry.deregister(worker.id())?;
        }

        Err(ServerError::worker_dispatch(
            activity.namespace.clone(),
            activity.activity_type.clone(),
            "all matching worker streams closed before task could be delivered",
        ))
    }
}

/// Decoded activity outcome reported by a worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityCompletionOutcome {
    /// Activity completed successfully with an output payload.
    Succeeded(Payload),
    /// Activity failed, preserving retryability classification for the engine.
    Failed(ActivityError),
}

/// Correlated activity completion handed to the engine-owned activity contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityCompletion {
    /// Owning workflow id.
    pub workflow_id: WorkflowId,
    /// Correlating activity id.
    pub activity_id: ActivityId,
    /// Worker-reported outcome.
    pub outcome: ActivityCompletionOutcome,
}

impl TryFrom<ProtoActivityResult> for ActivityCompletion {
    type Error = ServerError;

    fn try_from(value: ProtoActivityResult) -> Result<Self, Self::Error> {
        let workflow_id = value
            .workflow_id
            .ok_or_else(|| wire_error("activity result workflow id is missing"))
            .and_then(|id| WorkflowId::try_from(id).map_err(ServerError::from))?;
        let activity_id = value
            .activity_id
            .ok_or_else(|| wire_error("activity result activity id is missing"))
            .map(ActivityId::from)?;
        let outcome = match value.outcome {
            Some(proto_activity_result::Outcome::Result(payload)) => {
                ActivityCompletionOutcome::Succeeded(
                    Payload::try_from(payload).map_err(ServerError::from)?,
                )
            }
            Some(proto_activity_result::Outcome::Error(error)) => {
                ActivityCompletionOutcome::Failed(
                    ActivityError::try_from(error).map_err(ServerError::from)?,
                )
            }
            None => return Err(wire_error("activity result outcome is missing")),
        };

        Ok(Self {
            workflow_id,
            activity_id,
            outcome,
        })
    }
}

/// Engine-owned activity completion contract used by the worker endpoint.
pub trait ActivityCompletionSink {
    /// Feed one worker-reported result into the engine activity contract.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the engine rejects or cannot record the completion.
    fn complete_activity(&self, completion: ActivityCompletion) -> Result<(), ServerError>;
}

/// Decode and hand a worker result to the engine-owned activity completion sink.
///
/// # Errors
///
/// Returns [`ServerError`] for malformed wire results or sink failures.
pub fn handle_activity_result(
    sink: &impl ActivityCompletionSink,
    result: ProtoActivityResult,
) -> Result<(), ServerError> {
    sink.complete_activity(ActivityCompletion::try_from(result)?)
}

fn wire_error(message: &'static str) -> ServerError {
    ServerError::Wire {
        wire: WireError::backend(message),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use aion_core::{ActivityErrorKind, ContentType};
    use aion_proto::{ProtoActivityError, ProtoActivityErrorKind};
    use serde_json::json;
    use uuid::Uuid;

    use crate::worker::registry::ConnectedWorkerRegistry;

    use super::*;

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(Uuid::nil())
    }

    fn activity_id() -> ActivityId {
        ActivityId::from_sequence_position(42)
    }

    fn payload(value: serde_json::Value) -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(&value)?)
    }

    #[tokio::test]
    async fn dispatch_pushes_activity_task_with_correlation()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let activity_types = [String::from("charge-card")];
        let registration = registry.register("tenant-a", activity_types.iter(), tx)?;
        let dispatcher = ActivityDispatcher::new(registry.clone());
        let input = payload(json!({"amount": 1200}))?;
        let scheduled = ScheduledActivity {
            namespace: String::from("tenant-a"),
            activity_type: String::from("charge-card"),
            workflow_id: workflow_id(),
            activity_id: activity_id(),
            input: input.clone(),
        };

        dispatcher.dispatch(&scheduled).await?;
        let task = rx.recv().await.ok_or("expected pushed activity task")?;

        assert_eq!(task.workflow_id, Some(ProtoWorkflowId::from(workflow_id())));
        assert_eq!(task.activity_id, Some(ProtoActivityId::from(activity_id())));
        assert_eq!(task.activity_type, "charge-card");
        assert_eq!(task.input, Some(ProtoPayload::from(input)));

        registration.deregister()?;
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_without_matching_worker_reports_unplaced() -> Result<(), ServerError> {
        let dispatcher = ActivityDispatcher::new(ConnectedWorkerRegistry::default());
        let scheduled = ScheduledActivity {
            namespace: String::from("tenant-a"),
            activity_type: String::from("charge-card"),
            workflow_id: workflow_id(),
            activity_id: activity_id(),
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
        };

        let result = dispatcher.dispatch(&scheduled).await;
        assert!(matches!(result, Err(ServerError::WorkerDispatch { .. })));
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_skips_closed_worker_and_uses_next_match()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);
        let (live_tx, mut live_rx) = tokio::sync::mpsc::channel(1);
        let activity_types = [String::from("charge-card")];
        let closed_registration =
            registry.register("tenant-a", activity_types.iter(), closed_tx)?;
        let live_registration = registry.register("tenant-a", activity_types.iter(), live_tx)?;
        drop(closed_rx);

        let dispatcher = ActivityDispatcher::new(registry.clone());
        let scheduled = ScheduledActivity {
            namespace: String::from("tenant-a"),
            activity_type: String::from("charge-card"),
            workflow_id: workflow_id(),
            activity_id: activity_id(),
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
        };

        dispatcher.dispatch(&scheduled).await?;

        assert!(live_rx.recv().await.is_some());
        assert_eq!(registry.workers_for("tenant-a", "charge-card")?.len(), 1);

        closed_registration.deregister()?;
        live_registration.deregister()?;
        Ok(())
    }

    #[derive(Default)]
    struct RecordingSink {
        completions: Mutex<Vec<ActivityCompletion>>,
    }

    impl ActivityCompletionSink for RecordingSink {
        fn complete_activity(&self, completion: ActivityCompletion) -> Result<(), ServerError> {
            self.completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .push(completion);
            Ok(())
        }
    }

    #[test]
    fn successful_activity_result_calls_completion_sink() -> Result<(), Box<dyn std::error::Error>>
    {
        let sink = RecordingSink::default();
        let output = payload(json!({"ok": true}))?;
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(activity_id())),
            outcome: Some(proto_activity_result::Outcome::Result(ProtoPayload::from(
                output.clone(),
            ))),
        };

        handle_activity_result(&sink, result)?;
        let completions = sink
            .completions
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?;

        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].workflow_id, workflow_id());
        assert_eq!(completions[0].activity_id, activity_id());
        assert_eq!(
            completions[0].outcome,
            ActivityCompletionOutcome::Succeeded(output)
        );
        Ok(())
    }

    #[test]
    fn failed_activity_result_preserves_error_classification()
    -> Result<(), Box<dyn std::error::Error>> {
        let sink = RecordingSink::default();
        let error = ProtoActivityError {
            kind: ProtoActivityErrorKind::Retryable as i32,
            message: String::from("temporary outage"),
            details: Some(ProtoPayload::from(payload(json!({"retry_after_ms": 500}))?)),
        };
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(activity_id())),
            outcome: Some(proto_activity_result::Outcome::Error(error)),
        };

        handle_activity_result(&sink, result)?;
        let completions = sink
            .completions
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?;

        assert_eq!(completions.len(), 1);
        match &completions[0].outcome {
            ActivityCompletionOutcome::Failed(error) => {
                assert_eq!(error.kind, ActivityErrorKind::Retryable);
                assert!(error.is_retryable());
            }
            ActivityCompletionOutcome::Succeeded(_) => return Err("expected failed outcome".into()),
        }
        Ok(())
    }
}
