//! Backoff reconnect, re-register, and re-report un-acked results.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;

use aion_core::{ActivityError, ActivityId, Payload, WorkflowId};
use tracing::{debug, error, warn};

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::protocol::{GrpcWorkerSession, WorkerSession};

/// Result or failure computed locally and not yet acknowledged by the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingActivityReport {
    /// Successful activity output to re-report until acknowledged.
    Completed {
        /// Workflow owning the activity.
        workflow_id: WorkflowId,
        /// Activity identifier used by AW for idempotent ingest.
        activity_id: ActivityId,
        /// Opaque activity output payload.
        output: Payload,
    },
    /// Explicitly classified activity failure to re-report until acknowledged.
    Failed {
        /// Workflow owning the activity.
        workflow_id: WorkflowId,
        /// Activity identifier used by AW for idempotent ingest.
        activity_id: ActivityId,
        /// Classified activity error.
        failure: ActivityError,
    },
}

impl PendingActivityReport {
    /// Returns the report's activity id key.
    #[must_use]
    pub const fn activity_id(&self) -> &ActivityId {
        match self {
            Self::Completed { activity_id, .. } | Self::Failed { activity_id, .. } => activity_id,
        }
    }
}

/// In-memory source of truth for locally reported results awaiting engine ack.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnackedResultTracker {
    reports: BTreeMap<u64, PendingActivityReport>,
}

impl UnackedResultTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            reports: BTreeMap::new(),
        }
    }

    /// Records a report, replacing any earlier pending report for the same id.
    pub fn record(&mut self, report: PendingActivityReport) {
        self.reports
            .insert(report.activity_id().sequence_position(), report);
    }

    /// Drops a report once the engine explicitly acknowledges it.
    pub fn acknowledge(&mut self, activity_id: &ActivityId) -> Option<PendingActivityReport> {
        self.reports.remove(&activity_id.sequence_position())
    }

    /// Returns the number of unacknowledged reports.
    #[must_use]
    pub fn len(&self) -> usize {
        self.reports.len()
    }

    /// Returns true when no reports are waiting for acknowledgement.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reports.is_empty()
    }

    /// Gets a pending report by activity id.
    #[must_use]
    pub fn get(&self, activity_id: &ActivityId) -> Option<&PendingActivityReport> {
        self.reports.get(&activity_id.sequence_position())
    }

    /// Returns a deterministic snapshot for re-reporting without holding a borrow.
    #[must_use]
    pub fn snapshot(&self) -> Vec<PendingActivityReport> {
        self.reports.values().cloned().collect()
    }
}

/// Validated reconnect backoff settings drawn from [`WorkerConfig`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconnectBackoff {
    initial: Duration,
    max: Duration,
    attempts: usize,
}

impl ReconnectBackoff {
    /// Builds reconnect backoff from worker config.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError::Registration`] if delays or attempt counts are zero.
    pub fn from_config(config: &WorkerConfig) -> Result<Self, WorkerError> {
        if config.reconnect.initial_backoff.is_zero() {
            return Err(WorkerError::registration(InvalidReconnectBackoff {
                message: String::from("reconnect initial_backoff must be greater than zero"),
            }));
        }
        if config.reconnect.max_backoff.is_zero() {
            return Err(WorkerError::registration(InvalidReconnectBackoff {
                message: String::from("reconnect max_backoff must be greater than zero"),
            }));
        }
        if config.reconnect.max_attempts == 0 {
            return Err(WorkerError::registration(InvalidReconnectBackoff {
                message: String::from("reconnect max_attempts must be greater than zero"),
            }));
        }
        Ok(Self {
            initial: config.reconnect.initial_backoff,
            max: config.reconnect.max_backoff,
            attempts: config.reconnect.max_attempts,
        })
    }

    fn delay_for_attempt(&self, completed_failures: usize) -> Duration {
        let bounded_shift = completed_failures.saturating_sub(1).min(31);
        let shift = u32::try_from(bounded_shift).map_or(31, |shift| shift);
        let factor = 1_u32.checked_shl(shift).map_or(u32::MAX, |factor| factor);
        self.initial.saturating_mul(factor).min(self.max)
    }

    fn attempts(&self) -> usize {
        self.attempts
    }
}

/// Connects, handshakes, and registers a fresh gRPC worker session.
///
/// # Errors
///
/// Returns [`WorkerError`] if connection, handshake, or registration fails.
pub async fn connect_registered_grpc_session(
    config: &WorkerConfig,
    activity_types: Vec<String>,
    available_handlers: &BTreeSet<String>,
) -> Result<GrpcWorkerSession, WorkerError> {
    let session = GrpcWorkerSession::connect(config.clone()).await?;
    register_connected_session(session, config, activity_types, available_handlers).await
}

/// Handshakes and registers an already-connected session.
///
/// # Errors
///
/// Returns [`WorkerError`] if handshake or registration fails.
pub async fn register_connected_session<S>(
    mut session: S,
    config: &WorkerConfig,
    activity_types: Vec<String>,
    available_handlers: &BTreeSet<String>,
) -> Result<S, WorkerError>
where
    S: WorkerSession,
{
    session.handshake(config).await?;
    session.register(activity_types, available_handlers).await?;
    Ok(session)
}

/// Reconnects with bounded exponential backoff using an injected session factory.
///
/// # Errors
///
/// Returns the last [`WorkerError`] after configured attempts are exhausted or if
/// the config contains invalid zero reconnect settings.
pub async fn reconnect_with_backoff<S, F, Fut>(
    config: &WorkerConfig,
    activity_types: Vec<String>,
    available_handlers: &BTreeSet<String>,
    connect: F,
) -> Result<S, WorkerError>
where
    S: WorkerSession,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<S, WorkerError>>,
{
    reconnect_with_sleep(
        config,
        activity_types,
        available_handlers,
        connect,
        tokio::time::sleep,
    )
    .await
}

/// Testable reconnect helper with injectable sleep.
///
/// # Errors
///
/// Returns the last [`WorkerError`] after configured attempts are exhausted or if
/// the config contains invalid zero reconnect settings.
pub async fn reconnect_with_sleep<S, F, Fut, Sleep, SleepFut>(
    config: &WorkerConfig,
    activity_types: Vec<String>,
    available_handlers: &BTreeSet<String>,
    mut connect: F,
    mut sleep: Sleep,
) -> Result<S, WorkerError>
where
    S: WorkerSession,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<S, WorkerError>>,
    Sleep: FnMut(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
{
    let backoff = ReconnectBackoff::from_config(config)?;
    let mut last_error = None;

    for attempt in 1..=backoff.attempts() {
        debug!(attempt, "attempting worker reconnect");
        let result = match connect().await {
            Ok(session) => {
                register_connected_session(
                    session,
                    config,
                    activity_types.clone(),
                    available_handlers,
                )
                .await
            }
            Err(error) => Err(error),
        };

        match result {
            Ok(session) => {
                debug!(attempt, "worker reconnect succeeded");
                return Ok(session);
            }
            Err(error) => {
                if attempt == backoff.attempts() {
                    error!(attempt, error = %error, "worker reconnect attempts exhausted");
                    return Err(error);
                }
                let delay = backoff.delay_for_attempt(attempt);
                warn!(
                    attempt,
                    delay_ms = delay.as_millis(),
                    error = %error,
                    "worker reconnect failed; backing off"
                );
                last_error = Some(error);
                sleep(delay).await;
            }
        }
    }

    let _ = last_error;
    Err(WorkerError::registration(InvalidReconnectBackoff {
        message: String::from("reconnect_max_attempts must be greater than zero"),
    }))
}

/// Re-reports every unacknowledged result/failure before serving new work.
///
/// # Errors
///
/// Returns [`WorkerError`] if any re-report send fails. Entries are not removed;
/// only explicit acknowledgement may clear the tracker.
pub async fn re_report_unacked<S>(
    tracker: &UnackedResultTracker,
    session: &mut S,
) -> Result<(), WorkerError>
where
    S: WorkerSession,
{
    for report in tracker.snapshot() {
        match report {
            PendingActivityReport::Completed {
                workflow_id,
                activity_id,
                output,
            } => {
                debug!(
                    activity_id = activity_id.sequence_position(),
                    "re-reporting unacknowledged activity result"
                );
                session
                    .report_result(workflow_id, activity_id, output)
                    .await?;
            }
            PendingActivityReport::Failed {
                workflow_id,
                activity_id,
                failure,
            } => {
                debug!(
                    activity_id = activity_id.sequence_position(),
                    "re-reporting unacknowledged activity failure"
                );
                session
                    .report_failure(workflow_id, activity_id, failure)
                    .await?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct InvalidReconnectBackoff {
    message: String,
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;
    use std::time::Duration;

    use aion_core::{
        ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload, WorkflowId,
    };
    use aion_proto::ProtoActivityTask;
    use async_trait::async_trait;
    use futures::stream;

    use super::{
        PendingActivityReport, UnackedResultTracker, re_report_unacked, reconnect_with_sleep,
    };
    use crate::error::WorkerError;
    use crate::protocol::{WorkerSession, WorkerTaskStream, validate_activity_handlers};
    use crate::{ReconnectConfig, WorkerConfig};

    #[test]
    fn tracker_records_reports_and_acknowledges_by_activity_id() {
        let workflow_id = WorkflowId::new_v4();
        let first_id = ActivityId::from_sequence_position(1);
        let second_id = ActivityId::from_sequence_position(2);
        let mut tracker = UnackedResultTracker::new();

        tracker.record(PendingActivityReport::Completed {
            workflow_id: workflow_id.clone(),
            activity_id: first_id.clone(),
            output: Payload::new(ContentType::Json, b"{\"first\":true}".to_vec()),
        });
        tracker.record(PendingActivityReport::Completed {
            workflow_id,
            activity_id: second_id.clone(),
            output: Payload::new(ContentType::Json, b"{\"second\":true}".to_vec()),
        });

        assert_eq!(tracker.len(), 2);
        assert!(tracker.acknowledge(&first_id).is_some());
        assert_eq!(tracker.len(), 1);
        assert!(tracker.get(&second_id).is_some());
        assert!(tracker.get(&first_id).is_none());
    }

    #[tokio::test]
    async fn reconnect_fails_once_then_handshakes_and_registers() -> Result<(), WorkerError> {
        let config = test_config();
        let attempts = Rc::new(RefCell::new(0usize));
        let sleeps = Rc::new(RefCell::new(Vec::new()));
        let activity_types = vec![String::from("charge-card")];
        let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();
        let attempts_for_connect = Rc::clone(&attempts);
        let sleeps_for_sleep = Rc::clone(&sleeps);

        let session = reconnect_with_sleep(
            &config,
            activity_types.clone(),
            &handlers,
            move || {
                let attempts_for_connect = Rc::clone(&attempts_for_connect);
                async move {
                    let mut attempts = attempts_for_connect.borrow_mut();
                    *attempts += 1;
                    if *attempts == 1 {
                        Err(WorkerError::Transport {
                            source: tonic::Status::unavailable("disconnected"),
                        })
                    } else {
                        Ok(ReconnectFakeSession::default())
                    }
                }
            },
            move |delay| {
                let sleeps_for_sleep = Rc::clone(&sleeps_for_sleep);
                async move {
                    sleeps_for_sleep.borrow_mut().push(delay);
                }
            },
        )
        .await?;

        assert_eq!(*attempts.borrow(), 2);
        assert_eq!(*sleeps.borrow(), vec![Duration::from_millis(5)]);
        assert_eq!(session.handshakes, vec![String::from("worker-a")]);
        assert_eq!(session.registrations, vec![activity_types]);
        Ok(())
    }

    #[tokio::test]
    async fn re_reports_unacked_reports_without_removing_them() -> Result<(), WorkerError> {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(7);
        let output = Payload::new(ContentType::Json, b"{}".to_vec());
        let mut tracker = UnackedResultTracker::new();
        tracker.record(PendingActivityReport::Completed {
            workflow_id: workflow_id.clone(),
            activity_id: activity_id.clone(),
            output: output.clone(),
        });
        let mut session = ReconnectFakeSession::default();

        re_report_unacked(&tracker, &mut session).await?;

        assert_eq!(tracker.len(), 1);
        assert_eq!(
            session.reports,
            vec![RecordedReport::Completed(workflow_id, activity_id, output)]
        );
        Ok(())
    }

    #[derive(Default)]
    struct ReconnectFakeSession {
        handshakes: Vec<String>,
        registrations: Vec<Vec<String>>,
        reports: Vec<RecordedReport>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum RecordedReport {
        Completed(WorkflowId, ActivityId, Payload),
        Failed(WorkflowId, ActivityId, ActivityError),
    }

    #[async_trait]
    impl WorkerSession for ReconnectFakeSession {
        async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
            self.handshakes.push(config.identity.clone());
            Ok(())
        }

        async fn register(
            &mut self,
            activity_types: Vec<String>,
            available_handlers: &BTreeSet<String>,
        ) -> Result<(), WorkerError> {
            validate_activity_handlers(&activity_types, available_handlers)?;
            self.registrations.push(activity_types);
            Ok(())
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            Box::pin(stream::empty::<Result<ProtoActivityTask, WorkerError>>())
        }

        async fn report_result(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            result: Payload,
        ) -> Result<(), WorkerError> {
            self.reports
                .push(RecordedReport::Completed(workflow_id, activity_id, result));
            Ok(())
        }

        async fn report_failure(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            failure: ActivityError,
        ) -> Result<(), WorkerError> {
            self.reports
                .push(RecordedReport::Failed(workflow_id, activity_id, failure));
            Ok(())
        }

        async fn send_heartbeat(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            progress: Option<Payload>,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, progress));
            Ok(())
        }
    }

    fn test_config() -> WorkerConfig {
        WorkerConfig::new(
            "http://127.0.0.1:50051",
            "payments",
            "worker-a",
            2,
            ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(20), 3),
            None,
        )
    }

    fn terminal_failure() -> ActivityError {
        ActivityError {
            kind: ActivityErrorKind::Terminal,
            message: String::from("terminal"),
            details: None,
        }
    }

    #[test]
    fn tracker_replaces_existing_activity_report() {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(9);
        let mut tracker = UnackedResultTracker::new();
        tracker.record(PendingActivityReport::Completed {
            workflow_id: workflow_id.clone(),
            activity_id: activity_id.clone(),
            output: Payload::new(ContentType::Json, b"{}".to_vec()),
        });
        tracker.record(PendingActivityReport::Failed {
            workflow_id,
            activity_id: activity_id.clone(),
            failure: terminal_failure(),
        });

        assert_eq!(tracker.len(), 1);
        assert!(matches!(
            tracker.get(&activity_id),
            Some(PendingActivityReport::Failed { .. })
        ));
    }
}
