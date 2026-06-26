//! Backoff reconnect, re-register, and re-report un-acked results.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;

use aion_core::{ActivityError, ActivityId, Payload, RunId, WorkflowId};
use tracing::{debug, error, warn};
use uuid::Uuid;

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
        /// Concrete workflow run to echo on re-report, when known.
        run_id: Option<RunId>,
        /// Opaque activity output payload.
        output: Payload,
    },
    /// Explicitly classified activity failure to re-report until acknowledged.
    Failed {
        /// Workflow owning the activity.
        workflow_id: WorkflowId,
        /// Activity identifier used by AW for idempotent ingest.
        activity_id: ActivityId,
        /// Concrete workflow run to echo on re-report, when known.
        run_id: Option<RunId>,
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

    /// Returns the workflow owning the report's activity.
    #[must_use]
    pub const fn workflow_id(&self) -> &WorkflowId {
        match self {
            Self::Completed { workflow_id, .. } | Self::Failed { workflow_id, .. } => workflow_id,
        }
    }
}

/// Deterministic tracker key: activity ids are sequence positions scoped to
/// one workflow, so distinct workflows legitimately collide on the bare
/// position and must be keyed by workflow as well.
type PendingReportKey = (Uuid, u64);

fn pending_report_key(workflow_id: &WorkflowId, activity_id: &ActivityId) -> PendingReportKey {
    (workflow_id.as_uuid(), activity_id.sequence_position())
}

/// In-memory source of truth for locally reported results awaiting engine ack.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnackedResultTracker {
    reports: BTreeMap<PendingReportKey, PendingActivityReport>,
}

impl UnackedResultTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            reports: BTreeMap::new(),
        }
    }

    /// Records a report, replacing any earlier pending report for the same
    /// workflow and activity id.
    pub fn record(&mut self, report: PendingActivityReport) {
        let key = pending_report_key(report.workflow_id(), report.activity_id());
        self.reports.insert(key, report);
    }

    /// Drops a report once the engine explicitly acknowledges it.
    pub fn acknowledge(
        &mut self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Option<PendingActivityReport> {
        self.reports
            .remove(&pending_report_key(workflow_id, activity_id))
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

    /// Gets a pending report by its workflow and activity id.
    #[must_use]
    pub fn get(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Option<&PendingActivityReport> {
        self.reports
            .get(&pending_report_key(workflow_id, activity_id))
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

    /// Returns the bounded exponential delay after `completed_failures` failures.
    ///
    /// The delay doubles per completed failure starting from the configured
    /// initial backoff and is capped at the configured maximum backoff.
    #[must_use]
    pub fn delay_for_attempt(&self, completed_failures: usize) -> Duration {
        let bounded_shift = completed_failures.saturating_sub(1).min(31);
        let shift = u32::try_from(bounded_shift).map_or(31, |shift| shift);
        let factor = 1_u32.checked_shl(shift).map_or(u32::MAX, |factor| factor);
        self.initial.saturating_mul(factor).min(self.max)
    }

    /// Returns the configured maximum number of reconnect attempts.
    #[must_use]
    pub const fn attempts(&self) -> usize {
        self.attempts
    }

    /// Returns the configured maximum backoff delay cap.
    ///
    /// The run loop also uses this as its session-health threshold: the cap
    /// is the policy's own definition of the longest pause, so an
    /// established session that survives longer than it is demonstrably past
    /// the flapping regime and resets the cumulative drop budget when it
    /// eventually drops.
    #[must_use]
    pub const fn max_delay(&self) -> Duration {
        self.max
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
/// Returns the last [`WorkerError`] after configured attempts are exhausted, or
/// immediately when the failure is a non-retryable `PermissionDenied` /
/// `Unauthenticated` denial, or if the config contains invalid zero reconnect
/// settings.
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
/// Returns the last [`WorkerError`] after configured attempts are exhausted, or
/// immediately — without consuming further attempts — when a failure is a
/// non-retryable denial ([`WorkerError::is_retryable`] is false, i.e. the
/// server answered `PermissionDenied` or `Unauthenticated`), or if the config
/// contains invalid zero reconnect settings.
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
                if !error.is_retryable() {
                    error!(
                        attempt,
                        error = %error,
                        "worker reconnect denied by server; not retrying"
                    );
                    return Err(error);
                }
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
                sleep(delay).await;
            }
        }
    }

    Err(WorkerError::registration(InvalidReconnectBackoff {
        message: String::from("reconnect_max_attempts must be greater than zero"),
    }))
}

/// Re-reports every unacknowledged result/failure before serving new work.
///
/// Server `ResultAck` frames clear entries mid-session, so the steady-state
/// backlog is empty and this replay decays to the still-unacked residue.
/// Each send carries the session's per-send deadline.
///
/// # Errors
///
/// Returns [`WorkerError`] if any re-report send fails. Entries are not removed
/// by sending; only the explicit `ResultAck` acknowledgement clears the tracker.
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
                run_id,
                output,
            } => {
                debug!(
                    workflow_id = %workflow_id,
                    activity_id = activity_id.sequence_position(),
                    "re-reporting unacknowledged activity result"
                );
                session
                    .report_result(workflow_id, activity_id, run_id, output)
                    .await?;
            }
            PendingActivityReport::Failed {
                workflow_id,
                activity_id,
                run_id,
                failure,
            } => {
                debug!(
                    workflow_id = %workflow_id,
                    activity_id = activity_id.sequence_position(),
                    "re-reporting unacknowledged activity failure"
                );
                session
                    .report_failure(workflow_id, activity_id, run_id, failure)
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
    use async_trait::async_trait;
    use futures::stream;

    use super::{
        PendingActivityReport, UnackedResultTracker, re_report_unacked, reconnect_with_sleep,
    };
    use crate::error::WorkerError;
    use crate::protocol::{
        WorkerSession, WorkerSessionEvent, WorkerTaskStream, validate_activity_handlers,
    };
    use crate::{ReconnectConfig, WorkerConfig};

    #[test]
    fn tracker_records_reports_and_acknowledges_by_workflow_and_activity_id() {
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
            workflow_id: workflow_id.clone(),
            activity_id: second_id.clone(),
            output: Payload::new(ContentType::Json, b"{\"second\":true}".to_vec()),
        });

        assert_eq!(tracker.len(), 2);
        assert!(tracker.acknowledge(&workflow_id, &first_id).is_some());
        assert_eq!(tracker.len(), 1);
        assert!(tracker.get(&workflow_id, &second_id).is_some());
        assert!(tracker.get(&workflow_id, &first_id).is_none());
    }

    #[test]
    fn tracker_keeps_reports_for_distinct_workflows_at_the_same_sequence_position() {
        let first_workflow = WorkflowId::new_v4();
        let second_workflow = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(3);
        let mut tracker = UnackedResultTracker::new();

        tracker.record(PendingActivityReport::Completed {
            workflow_id: first_workflow.clone(),
            activity_id: activity_id.clone(),
            output: Payload::new(ContentType::Json, b"{\"workflow\":\"a\"}".to_vec()),
        });
        tracker.record(PendingActivityReport::Completed {
            workflow_id: second_workflow.clone(),
            activity_id: activity_id.clone(),
            output: Payload::new(ContentType::Json, b"{\"workflow\":\"b\"}".to_vec()),
        });

        assert_eq!(tracker.len(), 2);
        assert!(tracker.get(&first_workflow, &activity_id).is_some());
        assert!(tracker.get(&second_workflow, &activity_id).is_some());
        assert!(
            tracker.acknowledge(&first_workflow, &activity_id).is_some(),
            "acknowledging one workflow's report must not require the other's"
        );
        assert_eq!(tracker.len(), 1);
        assert!(tracker.get(&second_workflow, &activity_id).is_some());
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
    async fn permission_denied_registration_stops_after_one_attempt() {
        let config = test_config();
        let attempts = Rc::new(RefCell::new(0usize));
        let sleeps = Rc::new(RefCell::new(Vec::new()));
        let activity_types = vec![String::from("charge-card")];
        let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();
        let attempts_for_connect = Rc::clone(&attempts);
        let sleeps_for_sleep = Rc::clone(&sleeps);

        let result = reconnect_with_sleep(
            &config,
            activity_types,
            &handlers,
            move || {
                let attempts_for_connect = Rc::clone(&attempts_for_connect);
                async move {
                    *attempts_for_connect.borrow_mut() += 1;
                    Ok(DeniedRegistrationSession {
                        denial: tonic::Status::permission_denied(
                            "namespace `payments` is not granted to subject `worker-a`",
                        ),
                    })
                }
            },
            move |delay| {
                let sleeps_for_sleep = Rc::clone(&sleeps_for_sleep);
                async move {
                    sleeps_for_sleep.borrow_mut().push(delay);
                }
            },
        )
        .await;

        assert!(result.is_err());
        let Err(error) = result else { return };
        assert_eq!(*attempts.borrow(), 1);
        assert!(sleeps.borrow().is_empty());
        assert!(!error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied)
        ));
        assert_eq!(
            error.grpc_status().map(tonic::Status::message),
            Some("namespace `payments` is not granted to subject `worker-a`")
        );
        assert!(
            error
                .to_string()
                .contains("namespace `payments` is not granted to subject `worker-a`")
        );
    }

    #[tokio::test]
    async fn unauthenticated_handshake_stops_after_one_attempt() {
        let config = test_config();
        let attempts = Rc::new(RefCell::new(0usize));
        let sleeps = Rc::new(RefCell::new(Vec::new()));
        let activity_types = vec![String::from("charge-card")];
        let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();
        let attempts_for_connect = Rc::clone(&attempts);
        let sleeps_for_sleep = Rc::clone(&sleeps);

        let result = reconnect_with_sleep(
            &config,
            activity_types,
            &handlers,
            move || {
                let attempts_for_connect = Rc::clone(&attempts_for_connect);
                async move {
                    *attempts_for_connect.borrow_mut() += 1;
                    Err::<ReconnectFakeSession, _>(WorkerError::Handshake {
                        source: tonic::Status::unauthenticated("worker credentials were rejected"),
                    })
                }
            },
            move |delay| {
                let sleeps_for_sleep = Rc::clone(&sleeps_for_sleep);
                async move {
                    sleeps_for_sleep.borrow_mut().push(delay);
                }
            },
        )
        .await;

        assert!(result.is_err());
        let Err(error) = result else { return };
        assert_eq!(*attempts.borrow(), 1);
        assert!(sleeps.borrow().is_empty());
        assert!(!error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::Unauthenticated)
        ));
        assert!(
            error
                .to_string()
                .contains("worker credentials were rejected")
        );
    }

    #[tokio::test]
    async fn unavailable_transport_retries_until_attempts_exhausted() {
        let config = test_config();
        let attempts = Rc::new(RefCell::new(0usize));
        let sleeps = Rc::new(RefCell::new(Vec::new()));
        let activity_types = vec![String::from("charge-card")];
        let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();
        let attempts_for_connect = Rc::clone(&attempts);
        let sleeps_for_sleep = Rc::clone(&sleeps);

        let result = reconnect_with_sleep(
            &config,
            activity_types,
            &handlers,
            move || {
                let attempts_for_connect = Rc::clone(&attempts_for_connect);
                async move {
                    *attempts_for_connect.borrow_mut() += 1;
                    Err::<ReconnectFakeSession, _>(WorkerError::Transport {
                        source: tonic::Status::unavailable("engine unreachable"),
                    })
                }
            },
            move |delay| {
                let sleeps_for_sleep = Rc::clone(&sleeps_for_sleep);
                async move {
                    sleeps_for_sleep.borrow_mut().push(delay);
                }
            },
        )
        .await;

        assert!(result.is_err());
        let Err(error) = result else { return };
        assert_eq!(*attempts.borrow(), 3);
        assert_eq!(
            *sleeps.borrow(),
            vec![Duration::from_millis(5), Duration::from_millis(10)]
        );
        assert!(error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::Unavailable)
        ));
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

    /// Session whose registration is rejected by the server with a gRPC denial,
    /// mirroring `aion-server` answering `PermissionDenied` for an ungranted
    /// namespace.
    struct DeniedRegistrationSession {
        denial: tonic::Status,
    }

    #[async_trait]
    impl WorkerSession for DeniedRegistrationSession {
        async fn handshake(&mut self, _config: &WorkerConfig) -> Result<(), WorkerError> {
            Ok(())
        }

        async fn register(
            &mut self,
            activity_types: Vec<String>,
            available_handlers: &BTreeSet<String>,
        ) -> Result<(), WorkerError> {
            validate_activity_handlers(&activity_types, available_handlers)?;
            Err(WorkerError::Registration {
                source: Box::new(self.denial.clone()),
            })
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            Box::pin(stream::empty::<Result<WorkerSessionEvent, WorkerError>>())
        }

        async fn report_result(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            result: Payload,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, result));
            Err(WorkerError::Registration {
                source: Box::new(self.denial.clone()),
            })
        }

        async fn report_failure(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            failure: ActivityError,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, failure));
            Err(WorkerError::Registration {
                source: Box::new(self.denial.clone()),
            })
        }

        async fn send_heartbeat(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            progress: Option<Payload>,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, progress));
            Err(WorkerError::Registration {
                source: Box::new(self.denial.clone()),
            })
        }
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
            Box::pin(stream::empty::<Result<WorkerSessionEvent, WorkerError>>())
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
            workflow_id: workflow_id.clone(),
            activity_id: activity_id.clone(),
            failure: terminal_failure(),
        });

        assert_eq!(tracker.len(), 1);
        assert!(matches!(
            tracker.get(&workflow_id, &activity_id),
            Some(PendingActivityReport::Failed { .. })
        ));
    }
}
