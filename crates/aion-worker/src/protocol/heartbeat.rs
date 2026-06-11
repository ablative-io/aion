//! heartbeat frame send + heartbeat-timeout bookkeeping

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aion_core::{ActivityId, WorkflowId};

use crate::context::HeartbeatRequest;
use crate::error::WorkerError;
use crate::protocol::WorkerSession;

/// In-memory liveness view for explicitly emitted activity heartbeats.
///
/// This bookkeeper is observability-only. It records the last successful local
/// send time for in-flight activities, but the SDK never enforces heartbeat
/// timeouts or fails activities for missing heartbeats; timeout ownership stays
/// with the engine.
#[derive(Clone, Debug, Default)]
pub struct HeartbeatBookkeeper {
    inner: Arc<Mutex<HashMap<ActivityExecutionKey, Option<Instant>>>>,
}

impl HeartbeatBookkeeper {
    /// Marks an activity execution as in flight without recording a heartbeat
    /// yet.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] if the in-memory bookkeeping mutex is poisoned.
    pub fn register(&self, key: ActivityExecutionKey) -> Result<(), WorkerError> {
        let mut last_heartbeats = self.lock_last_heartbeats()?;
        last_heartbeats.entry(key).or_insert(None);
        Ok(())
    }

    /// Removes bookkeeping for a completed activity execution.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] if the in-memory bookkeeping mutex is poisoned.
    pub fn remove(&self, key: &ActivityExecutionKey) -> Result<(), WorkerError> {
        let mut last_heartbeats = self.lock_last_heartbeats()?;
        last_heartbeats.remove(key);
        Ok(())
    }

    /// Returns the last successful local heartbeat send instant for an
    /// activity execution.
    #[must_use]
    pub fn last_heartbeat(&self, key: &ActivityExecutionKey) -> Option<Instant> {
        match self.inner.lock() {
            Ok(last_heartbeats) => last_heartbeats.get(key).copied().flatten(),
            Err(poisoned) => poisoned.into_inner().get(key).copied().flatten(),
        }
    }

    fn record_sent(&self, key: ActivityExecutionKey, sent_at: Instant) -> Result<(), WorkerError> {
        let mut last_heartbeats = self.lock_last_heartbeats()?;
        last_heartbeats.insert(key, Some(sent_at));
        Ok(())
    }

    fn lock_last_heartbeats(
        &self,
    ) -> Result<
        std::sync::MutexGuard<'_, HashMap<ActivityExecutionKey, Option<Instant>>>,
        WorkerError,
    > {
        self.inner
            .lock()
            .map_err(|_| WorkerError::registration(HeartbeatBookkeeperPoisoned))
    }
}

/// Sends one explicit heartbeat request and updates local liveness bookkeeping
/// after the transport accepts the frame.
///
/// # Errors
///
/// Returns [`WorkerError`] when the session send fails or bookkeeping cannot be
/// updated.
pub async fn send_heartbeat<S>(
    session: &mut S,
    bookkeeper: &HeartbeatBookkeeper,
    request: HeartbeatRequest,
) -> Result<(), WorkerError>
where
    S: WorkerSession,
{
    let key = ActivityExecutionKey::new(request.workflow_id.clone(), request.activity_id.clone());
    session
        .send_heartbeat(request.workflow_id, request.activity_id, request.detail)
        .await?;
    bookkeeper.record_sent(key, Instant::now())
}

#[derive(Debug, thiserror::Error)]
#[error("heartbeat bookkeeper mutex was poisoned")]
struct HeartbeatBookkeeperPoisoned;

/// Key identifying one in-flight activity execution.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActivityExecutionKey {
    /// Owning workflow id.
    pub workflow_id: WorkflowId,
    /// Activity id within the workflow.
    pub activity_id: ActivityId,
}

impl ActivityExecutionKey {
    /// Creates a key for an in-flight activity execution.
    #[must_use]
    pub const fn new(workflow_id: WorkflowId, activity_id: ActivityId) -> Self {
        Self {
            workflow_id,
            activity_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use aion_core::{ActivityError, ActivityId, ContentType, Payload, WorkflowId};
    use async_trait::async_trait;
    use futures::stream;

    use super::{ActivityExecutionKey, HeartbeatBookkeeper, send_heartbeat};
    use crate::WorkerConfig;
    use crate::context::HeartbeatRequest;
    use crate::error::WorkerError;
    use crate::protocol::{WorkerSession, WorkerTaskStream, validate_activity_handlers};

    #[derive(Debug, thiserror::Error)]
    #[error("heartbeat timestamp was not recorded")]
    struct MissingHeartbeatTimestamp;

    #[derive(Default)]
    struct FakeSession {
        heartbeats: Vec<RecordedHeartbeat>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RecordedHeartbeat {
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        detail: Option<Payload>,
    }

    #[async_trait]
    impl WorkerSession for FakeSession {
        async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
            drop(config.clone());
            Ok(())
        }

        async fn register(
            &mut self,
            activity_types: Vec<String>,
            available_handlers: &BTreeSet<String>,
        ) -> Result<(), WorkerError> {
            validate_activity_handlers(&activity_types, available_handlers)
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            Box::pin(stream::empty())
        }

        async fn report_result(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            result: Payload,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, result));
            Ok(())
        }

        async fn report_failure(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            failure: ActivityError,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, failure));
            Ok(())
        }

        async fn send_heartbeat(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            progress: Option<Payload>,
        ) -> Result<(), WorkerError> {
            self.heartbeats.push(RecordedHeartbeat {
                workflow_id,
                activity_id,
                detail: progress,
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn sends_explicit_heartbeats_and_preserves_detail() -> Result<(), WorkerError> {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(7);
        let detail = Payload::new(ContentType::Json, br#"{"progress":1}"#.to_vec());
        let bookkeeper = HeartbeatBookkeeper::default();
        let mut session = FakeSession::default();

        send_heartbeat(
            &mut session,
            &bookkeeper,
            HeartbeatRequest {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
                detail: Some(detail.clone()),
            },
        )
        .await?;
        send_heartbeat(
            &mut session,
            &bookkeeper,
            HeartbeatRequest {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
                detail: Some(detail.clone()),
            },
        )
        .await?;

        assert_eq!(
            session.heartbeats,
            vec![
                RecordedHeartbeat {
                    workflow_id: workflow_id.clone(),
                    activity_id: activity_id.clone(),
                    detail: Some(detail.clone()),
                },
                RecordedHeartbeat {
                    workflow_id,
                    activity_id,
                    detail: Some(detail.clone()),
                },
            ]
        );
        assert_eq!(detail.content_type(), &ContentType::Json);
        Ok(())
    }

    #[tokio::test]
    async fn last_heartbeat_timestamp_advances_on_each_send() -> Result<(), WorkerError> {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(8);
        let key = ActivityExecutionKey::new(workflow_id.clone(), activity_id.clone());
        let bookkeeper = HeartbeatBookkeeper::default();
        let mut session = FakeSession::default();

        send_heartbeat(
            &mut session,
            &bookkeeper,
            HeartbeatRequest {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
                detail: None,
            },
        )
        .await?;
        let first = bookkeeper.last_heartbeat(&key);
        tokio::time::sleep(Duration::from_millis(1)).await;
        send_heartbeat(
            &mut session,
            &bookkeeper,
            HeartbeatRequest {
                workflow_id,
                activity_id: activity_id.clone(),
                detail: None,
            },
        )
        .await?;
        let second = bookkeeper.last_heartbeat(&key);

        let (Some(first), Some(second)) = (first, second) else {
            return Err(WorkerError::decode(MissingHeartbeatTimestamp));
        };
        assert!(second > first);
        Ok(())
    }

    #[tokio::test]
    async fn colliding_sequence_positions_track_per_workflow() -> Result<(), WorkerError> {
        let activity_id = ActivityId::from_sequence_position(3);
        let workflow_a = WorkflowId::new_v4();
        let workflow_b = WorkflowId::new_v4();
        let key_a = ActivityExecutionKey::new(workflow_a.clone(), activity_id.clone());
        let key_b = ActivityExecutionKey::new(workflow_b.clone(), activity_id.clone());
        let bookkeeper = HeartbeatBookkeeper::default();
        let mut session = FakeSession::default();

        bookkeeper.register(key_a.clone())?;
        bookkeeper.register(key_b.clone())?;

        // record_sent for workflow A never touches workflow B's timestamp.
        send_heartbeat(
            &mut session,
            &bookkeeper,
            HeartbeatRequest {
                workflow_id: workflow_a,
                activity_id: activity_id.clone(),
                detail: None,
            },
        )
        .await?;
        assert!(bookkeeper.last_heartbeat(&key_a).is_some());
        assert!(bookkeeper.last_heartbeat(&key_b).is_none());

        send_heartbeat(
            &mut session,
            &bookkeeper,
            HeartbeatRequest {
                workflow_id: workflow_b,
                activity_id,
                detail: None,
            },
        )
        .await?;
        let b_before_a_completes = bookkeeper.last_heartbeat(&key_b);
        let Some(b_before_a_completes) = b_before_a_completes else {
            return Err(WorkerError::decode(MissingHeartbeatTimestamp));
        };

        // Completing workflow A's activity leaves workflow B's entry intact.
        bookkeeper.remove(&key_a)?;
        assert!(bookkeeper.last_heartbeat(&key_a).is_none());
        assert_eq!(
            bookkeeper.last_heartbeat(&key_b),
            Some(b_before_a_completes)
        );
        Ok(())
    }
}
