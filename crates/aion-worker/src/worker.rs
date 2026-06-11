//! `Worker` builder, run loop, and shutdown wiring.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::{error, info, warn};

use crate::activity::{ActivityRegistry, HandlerFuture};
use crate::config::WorkerConfig;
use crate::context::ActivityContext;
use crate::error::WorkerError;
use crate::protocol::reconnect::{
    ReconnectBackoff, UnackedResultTracker, re_report_unacked, reconnect_with_backoff,
    register_connected_session,
};
use crate::protocol::{GrpcWorkerSession, WorkerSession};
use crate::runtime::{NoShutdown, ServeEnd, serve_activity_tasks, serve_activity_tasks_until};

/// Builder for a configured worker and its registered typed activities.
#[must_use]
pub struct WorkerBuilder {
    config: WorkerConfig,
    activities: ActivityRegistry,
}

impl WorkerBuilder {
    /// Creates a builder for a worker using the supplied config.
    pub fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            activities: ActivityRegistry::new(),
        }
    }

    /// Registers one typed activity handler on the builder.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError::Registration`] when the activity type is duplicate.
    pub fn register_activity<Input, Output, Handler>(
        mut self,
        activity_type: impl Into<String>,
        handler: Handler,
    ) -> Result<Self, WorkerError>
    where
        Input: Serialize + DeserializeOwned + Send + Sync + 'static,
        Output: Serialize + Send + Sync + 'static,
        Handler: for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
            + Send
            + Sync
            + 'static,
    {
        self.activities = self.activities.register_activity(activity_type, handler)?;
        Ok(self)
    }

    /// Builds the worker after validating that it has at least one activity.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError::Registration`] when no activities are registered.
    pub fn build(self) -> Result<Worker, WorkerError> {
        if self.activities.is_empty() {
            return Err(WorkerError::registration(EmptyActivitySet));
        }
        let available_handlers = self.activities.activity_types();
        let activity_types = available_handlers.iter().cloned().collect();
        Ok(Worker {
            config: self.config,
            activity_types,
            available_handlers,
            activities: Arc::new(self.activities),
        })
    }
}

/// Configured Rust worker with typed activity handlers.
#[must_use]
pub struct Worker {
    config: WorkerConfig,
    activity_types: Vec<String>,
    available_handlers: BTreeSet<String>,
    activities: Arc<ActivityRegistry>,
}

impl Worker {
    /// Starts a new builder for the supplied config.
    pub fn builder(config: WorkerConfig) -> WorkerBuilder {
        WorkerBuilder::new(config)
    }

    /// Returns the activity types this worker registers with the engine.
    #[must_use]
    pub fn activity_types(&self) -> &[String] {
        &self.activity_types
    }

    /// Returns the handler-name set used for registration validation.
    #[must_use]
    pub fn available_handlers(&self) -> &BTreeSet<String> {
        &self.available_handlers
    }

    /// Connects to the configured endpoint, registers activities, and serves indefinitely.
    ///
    /// Session establishment goes through the bounded-backoff reconnect
    /// machinery configured in [`WorkerConfig::reconnect`], and retryable
    /// mid-run transport drops — including clean server-side stream closes —
    /// re-establish through the same machinery: the worker re-registers its
    /// activity types, re-reports every unacknowledged activity result (the
    /// engine ingests reports idempotently by `ActivityId`), and resumes
    /// serving. Deterministic `PermissionDenied` / `Unauthenticated` denials
    /// surface after exactly one attempt. Without a shutdown signal the run
    /// ends only on a non-retryable error or drop-budget exhaustion; see
    /// [`crate::config::ReconnectConfig`] for the budget-reset semantics.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] for connection, registration, dispatch, heartbeat, or report failures.
    pub async fn run(self) -> Result<(), WorkerError> {
        self.run_until(std::future::pending::<()>()).await
    }

    /// Connects to the configured endpoint, registers activities, and serves until shutdown fires.
    ///
    /// Establishment and mid-run reconnect behaviour match [`Worker::run`].
    /// On shutdown, no new tasks are pulled, in-flight activity contexts are
    /// marked cancelled, and all in-flight activities are drained before this
    /// returns; shutdown signalled during a reconnect or backoff wins
    /// promptly without waiting out the backoff delay.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] for connection, registration, dispatch, heartbeat, or report failures.
    pub async fn run_until<Shutdown>(self, shutdown: Shutdown) -> Result<(), WorkerError>
    where
        Shutdown: Future<Output = ()> + Send,
    {
        let config = self.config.clone();
        self.run_with_connector_until(move || GrpcWorkerSession::connect(config.clone()), shutdown)
            .await
    }

    /// Runs the reconnect-aware serve loop over an injected session factory.
    ///
    /// Session establishment goes through
    /// [`reconnect_with_backoff`](crate::protocol::reconnect::reconnect_with_backoff):
    /// transient failures retry up to the configured `reconnect.max_attempts`
    /// with bounded exponential backoff, while `PermissionDenied` /
    /// `Unauthenticated` denials surface after exactly one attempt. When an
    /// established session drops retryably mid-run — a retryable transport
    /// failure or a clean server-side stream close, both count — the worker
    /// drains in-flight activities into the unacked tracker, backs off,
    /// reconnects through the same machinery (re-registering its activity
    /// types), re-reports every unacknowledged result, and resumes serving.
    ///
    /// Mid-run drops share one cumulative budget of `reconnect.max_attempts`,
    /// matching the Python and TypeScript workers, and the budget resets to
    /// zero once a session proves healthy: it served at least one task, or it
    /// survived longer than `reconnect.max_backoff` (measured monotonically
    /// from successful registration to the drop). See
    /// [`crate::config::ReconnectConfig`]. The run therefore ends only on
    /// shutdown, a non-retryable error, or budget exhaustion — never merely
    /// because the server closed the stream. At most one session is alive at
    /// a time, and a shutdown signalled during a reconnect or backoff wins
    /// promptly (returning `Ok` when the pending drop was a clean close).
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] when establishment attempts are exhausted or
    /// denied, when a non-retryable error occurs mid-run, when the mid-run
    /// drop budget is exhausted ([`WorkerError::CleanCloseExhausted`] when
    /// the exhausting drops were clean closes), or when shutdown interrupts
    /// an unrecovered error drop.
    pub async fn run_with_connector_until<S, F, Fut, Shutdown>(
        self,
        mut connect: F,
        shutdown: Shutdown,
    ) -> Result<(), WorkerError>
    where
        S: WorkerSession,
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<S, WorkerError>>,
        Shutdown: Future<Output = ()> + Send,
    {
        let backoff = ReconnectBackoff::from_config(&self.config)?;
        let mut tracker = UnackedResultTracker::new();
        tokio::pin!(shutdown);
        let mut shutdown = SharedShutdown::new(shutdown);
        let mut drop_failures = 0_usize;
        let mut recovery_error: Option<WorkerError> = None;

        loop {
            let connected = tokio::select! {
                biased;
                () = shutdown.wait() => {
                    return recovery_error.take().map_or(Ok(()), Err);
                }
                result = reconnect_with_backoff(
                    &self.config,
                    self.activity_types.clone(),
                    &self.available_handlers,
                    &mut connect,
                ) => result,
            };
            let mut session = connected?;
            let session_started = tokio::time::Instant::now();
            let mut tasks_reported = 0_usize;
            let served = match re_report_unacked(&tracker, &mut session).await {
                Ok(()) => {
                    serve_activity_tasks_until(
                        &self.config,
                        &mut session,
                        Arc::clone(&self.activities),
                        &mut tracker,
                        &mut tasks_reported,
                        shutdown.wait(),
                    )
                    .await
                }
                Err(report_error) => Err(report_error),
            };
            drop(session);
            let cause = match served {
                Ok(ServeEnd::Shutdown) => return Ok(()),
                Ok(ServeEnd::StreamClosed) => {
                    if shutdown.fired() {
                        return Ok(());
                    }
                    DropCause::CleanClose
                }
                Err(error) if !error.is_retryable() => {
                    error!(error = %error, "worker session denied by server; not reconnecting");
                    return Err(error);
                }
                Err(error) => {
                    if shutdown.fired() {
                        return Err(error);
                    }
                    DropCause::Failure(error)
                }
            };
            let proved_healthy =
                tasks_reported > 0 || session_started.elapsed() > backoff.max_delay();
            if proved_healthy && drop_failures > 0 {
                info!(
                    drop_failures,
                    tasks_reported, "worker session proved healthy; drop budget reset"
                );
                drop_failures = 0;
            }
            drop_failures += 1;
            if drop_failures >= backoff.attempts() {
                let error = cause.into_exhaustion_error();
                error!(
                    drop_failures,
                    error = %error,
                    "worker session drop budget exhausted; not reconnecting"
                );
                return Err(error);
            }
            let delay = backoff.delay_for_attempt(drop_failures);
            warn!(
                drop_failures,
                delay_ms = delay.as_millis(),
                cause = %cause,
                "worker session dropped; reconnecting after backoff"
            );
            let shutdown_won = tokio::select! {
                biased;
                () = shutdown.wait() => true,
                () = tokio::time::sleep(delay) => false,
            };
            if shutdown_won {
                return cause.into_shutdown_result();
            }
            recovery_error = cause.into_recovery_error();
        }
    }

    /// Test seam that handshakes, registers, and serves an injected session until its stream ends.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] for registration, dispatch, heartbeat, or report failures.
    pub async fn run_with_session<S>(self, session: S) -> Result<S, WorkerError>
    where
        S: WorkerSession,
    {
        self.run_with_session_until(session, std::future::pending::<()>())
            .await
    }

    /// Test seam that handshakes, registers, and serves an injected session until shutdown fires.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] for registration, dispatch, heartbeat, or report failures.
    pub async fn run_with_session_until<S, Shutdown>(
        self,
        session: S,
        shutdown: Shutdown,
    ) -> Result<S, WorkerError>
    where
        S: WorkerSession,
        Shutdown: Future<Output = ()> + Send,
    {
        let mut session = register_connected_session(
            session,
            &self.config,
            self.activity_types.clone(),
            &self.available_handlers,
        )
        .await?;
        let mut tracker = UnackedResultTracker::new();
        let mut tasks_reported = 0_usize;
        serve_activity_tasks_until(
            &self.config,
            &mut session,
            self.activities,
            &mut tracker,
            &mut tasks_reported,
            shutdown,
        )
        .await?;
        Ok(session)
    }
}

/// Cause of a retryable mid-run session drop carried across one recovery cycle.
enum DropCause {
    /// The session ended with a retryable error.
    Failure(WorkerError),
    /// The server closed the stream cleanly while the run was still active.
    CleanClose,
}

impl DropCause {
    /// The classified error surfaced when this drop exhausts the budget.
    fn into_exhaustion_error(self) -> WorkerError {
        match self {
            Self::Failure(error) => error,
            Self::CleanClose => WorkerError::CleanCloseExhausted,
        }
    }

    /// Run outcome when shutdown wins the post-drop backoff: an error drop
    /// surfaces its error, a clean close is a graceful end.
    fn into_shutdown_result(self) -> Result<(), WorkerError> {
        match self {
            Self::Failure(error) => Err(error),
            Self::CleanClose => Ok(()),
        }
    }

    /// Error to surface if shutdown wins the recovery establishment select.
    fn into_recovery_error(self) -> Option<WorkerError> {
        match self {
            Self::Failure(error) => Some(error),
            Self::CleanClose => None,
        }
    }
}

impl std::fmt::Display for DropCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failure(error) => write!(formatter, "{error}"),
            Self::CleanClose => write!(formatter, "server closed the worker stream cleanly"),
        }
    }
}

/// Level-triggered, re-pollable view over the caller's one-shot shutdown future.
///
/// The run loop observes the same shutdown signal from several places —
/// session establishment, the serving loop, and reconnect backoff sleeps —
/// but a `Future` must not be polled again once it has completed. This
/// wrapper polls the underlying future at most to completion and then
/// latches, so every subsequent [`SharedShutdown::wait`] resolves
/// immediately.
struct SharedShutdown<'a, S> {
    inner: Pin<&'a mut S>,
    fired: bool,
}

impl<'a, S> SharedShutdown<'a, S>
where
    S: Future<Output = ()> + Send,
{
    const fn new(inner: Pin<&'a mut S>) -> Self {
        Self {
            inner,
            fired: false,
        }
    }

    /// Returns whether the shutdown future has already completed.
    const fn fired(&self) -> bool {
        self.fired
    }

    /// Waits for shutdown; resolves immediately once it has fired before.
    fn wait(&mut self) -> impl Future<Output = ()> + Send {
        std::future::poll_fn(|context| {
            if self.fired {
                return Poll::Ready(());
            }
            match self.inner.as_mut().poll(context) {
                Poll::Ready(()) => {
                    self.fired = true;
                    Poll::Ready(())
                }
                Poll::Pending => Poll::Pending,
            }
        })
    }
}

/// Connects and serves an already-built worker with the default non-shutdown future.
///
/// # Errors
///
/// Returns [`WorkerError`] for registration, dispatch, heartbeat, or report failures.
pub async fn run_worker_with_session<S>(worker: Worker, session: S) -> Result<S, WorkerError>
where
    S: WorkerSession,
{
    worker.run_with_session(session).await
}

/// Error returned when a worker is built without any registered activities.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("worker must register at least one activity handler")]
pub struct EmptyActivitySet;

fn _assert_live_session_type() {
    let _ = std::mem::size_of::<GrpcWorkerSession>();
    let _ = std::mem::size_of::<NoShutdown>();
    let _ = serve_activity_tasks::<GrpcWorkerSession, ActivityRegistry>;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use aion_core::{ActivityError, ActivityId, ContentType, Payload, WorkflowId};
    use aion_proto::{ProtoActivityId, ProtoActivityTask, ProtoPayload, ProtoWorkflowId};
    use async_trait::async_trait;
    use futures::StreamExt as _;
    use futures::stream;
    use serde::{Deserialize, Serialize};
    use tokio::sync::{Notify, mpsc};

    use super::{Worker, WorkerBuilder};
    use crate::config::{ReconnectConfig, WorkerConfig};
    use crate::context::ActivityContext;
    use crate::error::WorkerError;
    use crate::protocol::{
        WorkerSession, WorkerSessionEvent, WorkerTaskStream, validate_activity_handlers,
    };

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestInput {
        value: i32,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestOutput {
        value: i32,
    }

    struct ChannelSession {
        receiver: Option<mpsc::Receiver<Result<WorkerSessionEvent, WorkerError>>>,
        reports: Vec<RecordedReport>,
        registered: Vec<String>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum RecordedReport {
        Completed(WorkflowId, ActivityId, Payload),
        Failed(WorkflowId, ActivityId, ActivityError),
    }

    #[async_trait]
    impl WorkerSession for ChannelSession {
        async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
            drop(config.clone());
            Ok(())
        }

        async fn register(
            &mut self,
            activity_types: Vec<String>,
            available_handlers: &BTreeSet<String>,
        ) -> Result<(), WorkerError> {
            validate_activity_handlers(&activity_types, available_handlers)?;
            self.registered = activity_types;
            Ok(())
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            match self.receiver.take() {
                Some(receiver) => Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
                None => Box::pin(stream::empty()),
            }
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

    #[test]
    fn empty_worker_is_rejected() {
        let error = WorkerBuilder::new(test_config()).build().err();

        assert!(error.is_some_and(|error| error.to_string().contains("at least one activity")));
    }

    #[test]
    fn worker_collects_two_activity_registration_names() -> Result<(), WorkerError> {
        let worker = two_activity_worker()?;
        let expected = [String::from("double"), String::from("increment")]
            .into_iter()
            .collect::<BTreeSet<_>>();

        assert_eq!(worker.available_handlers(), &expected);
        assert_eq!(
            worker.activity_types(),
            &[String::from("double"), String::from("increment")]
        );
        Ok(())
    }

    #[tokio::test]
    async fn worker_registers_names_with_session() -> Result<(), WorkerError> {
        let worker = two_activity_worker()?;
        let session = worker
            .run_with_session(ChannelSession {
                receiver: None,
                reports: Vec::new(),
                registered: Vec::new(),
            })
            .await?;

        assert_eq!(
            session.registered,
            vec![String::from("double"), String::from("increment")]
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_waits_for_slow_in_flight_activity() -> Result<(), WorkerError> {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(7);
        let (sender, receiver) = mpsc::channel(2);
        sender
            .send(Ok(WorkerSessionEvent::Task(proto_task(
                workflow_id,
                activity_id.clone(),
                "slow",
                0,
            ))))
            .await
            .map_err(WorkerError::decode)?;
        let release = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));
        let worker = Worker::builder(test_config())
            .register_activity("slow", {
                let release = Arc::clone(&release);
                let started = Arc::clone(&started);
                move |input: TestInput, context: &ActivityContext| {
                    let release = Arc::clone(&release);
                    let started = Arc::clone(&started);
                    Box::pin(async move {
                        let _ = input;
                        started.fetch_add(1, Ordering::SeqCst);
                        context.cancelled().await;
                        while !release.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        Ok(TestOutput { value: 1 })
                    })
                }
            })?
            .build()?;
        let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        let session = ChannelSession {
            receiver: Some(receiver),
            reports: Vec::new(),
            registered: Vec::new(),
        };
        let handle = tokio::spawn(async move {
            worker
                .run_with_session_until(session, async {
                    let _ = shutdown_receiver.await;
                })
                .await
        });

        wait_until_started(&started).await;
        shutdown_sender
            .send(())
            .map_err(|()| WorkerError::decode(SendFailed))?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!handle.is_finished());
        release.store(true, Ordering::SeqCst);
        drop(sender);
        let session = handle.await.map_err(WorkerError::decode)??;

        assert_eq!(session.reports.len(), 1);
        assert!(matches!(
            &session.reports[0],
            RecordedReport::Completed(_, reported_id, _) if reported_id == &activity_id
        ));
        Ok(())
    }

    fn two_activity_worker() -> Result<Worker, WorkerError> {
        two_activity_worker_with(test_config())
    }

    fn two_activity_worker_with(config: WorkerConfig) -> Result<Worker, WorkerError> {
        Worker::builder(config)
            .register_activity("double", |input: TestInput, context| {
                Box::pin(async move {
                    let _ = context;
                    Ok(TestOutput {
                        value: input.value * 2,
                    })
                })
            })?
            .register_activity("increment", |input: TestInput, context| {
                Box::pin(async move {
                    let _ = context;
                    Ok(TestOutput {
                        value: input.value + 1,
                    })
                })
            })?
            .build()
    }

    fn proto_task(
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        activity_type: &str,
        value: i32,
    ) -> ProtoActivityTask {
        ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id)),
            activity_type: activity_type.to_owned(),
            input: Some(ProtoPayload::from(Payload::new(
                ContentType::Json,
                format!("{{\"value\":{value}}}").into_bytes(),
            ))),
        }
    }

    async fn wait_until_started(started: &AtomicUsize) {
        while started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    fn test_config() -> WorkerConfig {
        test_config_with(ReconnectConfig::new(
            Duration::from_millis(5),
            Duration::from_millis(20),
            3,
        ))
    }

    fn test_config_with(reconnect: ReconnectConfig) -> WorkerConfig {
        WorkerConfig::new(
            "http://127.0.0.1:50051",
            "payments",
            "worker-a",
            1,
            reconnect,
            None,
        )
    }

    fn slow_reconnect_config() -> WorkerConfig {
        test_config_with(ReconnectConfig::new(
            Duration::from_secs(5),
            Duration::from_secs(10),
            5,
        ))
    }

    #[derive(Debug, thiserror::Error)]
    #[error("failed to send shutdown signal")]
    struct SendFailed;

    #[derive(Debug, thiserror::Error)]
    #[error("expected the worker run to fail")]
    struct UnexpectedSuccess;

    #[derive(Debug, thiserror::Error)]
    #[error("expected a completed activity report")]
    struct UnexpectedReportShape;

    /// Per-session record emitted by [`ScriptedSession`] for post-run assertions.
    #[derive(Debug)]
    enum SessionLog {
        Registered(usize, Vec<String>),
        Reported(usize, RecordedReport),
    }

    /// Factory-injected session whose stream contents, registration outcome,
    /// and report behaviour are scripted per connection attempt.
    struct ScriptedSession {
        index: usize,
        log: mpsc::UnboundedSender<SessionLog>,
        events: Vec<Result<WorkerSessionEvent, WorkerError>>,
        fail_reports: bool,
        register_denial: Option<tonic::Status>,
        /// Delays the receive stream's first event so paused-clock tests can
        /// script a session that outlives the configured max backoff.
        delay_stream: Option<Duration>,
    }

    #[async_trait]
    impl WorkerSession for ScriptedSession {
        async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
            drop(config.clone());
            Ok(())
        }

        async fn register(
            &mut self,
            activity_types: Vec<String>,
            available_handlers: &BTreeSet<String>,
        ) -> Result<(), WorkerError> {
            validate_activity_handlers(&activity_types, available_handlers)?;
            if let Some(denial) = self.register_denial.take() {
                return Err(WorkerError::Registration {
                    source: Box::new(denial),
                });
            }
            self.log
                .send(SessionLog::Registered(self.index, activity_types))
                .map_err(WorkerError::decode)
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            let events = std::mem::take(&mut self.events);
            match self.delay_stream.take() {
                Some(delay) => Box::pin(
                    stream::once(async move {
                        tokio::time::sleep(delay).await;
                        stream::iter(events)
                    })
                    .flatten(),
                ),
                None => Box::pin(stream::iter(events)),
            }
        }

        async fn report_result(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            result: Payload,
        ) -> Result<(), WorkerError> {
            if self.fail_reports {
                return Err(WorkerError::Transport {
                    source: tonic::Status::unavailable("transport dropped before result ack"),
                });
            }
            self.log
                .send(SessionLog::Reported(
                    self.index,
                    RecordedReport::Completed(workflow_id, activity_id, result),
                ))
                .map_err(WorkerError::decode)
        }

        async fn report_failure(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            failure: ActivityError,
        ) -> Result<(), WorkerError> {
            if self.fail_reports {
                return Err(WorkerError::Transport {
                    source: tonic::Status::unavailable("transport dropped before failure ack"),
                });
            }
            self.log
                .send(SessionLog::Reported(
                    self.index,
                    RecordedReport::Failed(workflow_id, activity_id, failure),
                ))
                .map_err(WorkerError::decode)
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

    #[tokio::test]
    async fn establishment_retries_transient_failures_until_attempts_exhausted()
    -> Result<(), WorkerError> {
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    Err::<ScriptedSession, _>(WorkerError::Transport {
                        source: tonic::Status::unavailable("engine unreachable"),
                    })
                }
            }
        };

        let result = worker
            .run_with_connector_until(connect, std::future::pending::<()>())
            .await;

        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::Unavailable)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn establishment_denial_surfaces_after_one_attempt() -> Result<(), WorkerError> {
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let log = log_sender.clone();
                async move {
                    Ok(ScriptedSession {
                        index: 1,
                        log,
                        events: Vec::new(),
                        fail_reports: false,
                        register_denial: Some(tonic::Status::permission_denied(
                            "namespace `payments` is not granted to subject `worker-a`",
                        )),
                        delay_stream: None,
                    })
                }
            }
        };

        let result = worker
            .run_with_connector_until(connect, std::future::pending::<()>())
            .await;

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(!error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied)
        ));
        assert_eq!(
            error.grpc_status().map(tonic::Status::message),
            Some("namespace `payments` is not granted to subject `worker-a`")
        );
        drop(log_receiver);
        Ok(())
    }

    #[tokio::test]
    async fn mid_run_drop_reconnects_re_registers_and_re_reports_unacked() -> Result<(), WorkerError>
    {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(3);
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            let log_sender = log_sender.clone();
            let workflow_id = workflow_id.clone();
            let activity_id = activity_id.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                let task = proto_task(workflow_id.clone(), activity_id.clone(), "double", 21);
                async move {
                    if attempt == 1 {
                        Ok(ScriptedSession {
                            index: 1,
                            log,
                            events: vec![Ok(WorkerSessionEvent::Task(task))],
                            fail_reports: true,
                            register_denial: None,
                            delay_stream: None,
                        })
                    } else if attempt == 2 {
                        Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: Vec::new(),
                            fail_reports: false,
                            register_denial: None,
                            delay_stream: None,
                        })
                    } else {
                        // A clean close no longer ends the run, so the third
                        // establishment is denied deterministically to end it.
                        Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: Vec::new(),
                            fail_reports: false,
                            register_denial: Some(tonic::Status::permission_denied(
                                "namespace `payments` revoked for subject `worker-a`",
                            )),
                            delay_stream: None,
                        })
                    }
                }
            }
        };

        let result = worker
            .run_with_connector_until(connect, std::future::pending::<()>())
            .await;

        drop(log_sender);
        let mut registrations = Vec::new();
        let mut reports = Vec::new();
        while let Some(entry) = log_receiver.recv().await {
            match entry {
                SessionLog::Registered(index, types) => registrations.push((index, types)),
                SessionLog::Reported(index, report) => reports.push((index, report)),
            }
        }
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(!error.is_retryable());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let expected_types = vec![String::from("double"), String::from("increment")];
        assert_eq!(
            registrations,
            vec![(1, expected_types.clone()), (2, expected_types)]
        );
        assert_eq!(reports.len(), 1);
        let (session_index, report) = &reports[0];
        assert_eq!(*session_index, 2);
        let RecordedReport::Completed(reported_workflow, reported_id, payload) = report else {
            return Err(WorkerError::decode(UnexpectedReportShape));
        };
        assert_eq!(reported_workflow, &workflow_id);
        assert_eq!(reported_id, &activity_id);
        let output: TestOutput =
            serde_json::from_slice(payload.bytes()).map_err(WorkerError::decode)?;
        assert_eq!(output.value, 42);
        Ok(())
    }

    #[tokio::test]
    async fn mid_run_drop_re_reports_unacked_results_for_all_workflows() -> Result<(), WorkerError>
    {
        let first_workflow = WorkflowId::new_v4();
        let second_workflow = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(3);
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            let log_sender = log_sender.clone();
            let first_workflow = first_workflow.clone();
            let second_workflow = second_workflow.clone();
            let activity_id = activity_id.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                let first_task =
                    proto_task(first_workflow.clone(), activity_id.clone(), "double", 10);
                let second_task =
                    proto_task(second_workflow.clone(), activity_id.clone(), "double", 20);
                async move {
                    if attempt == 1 {
                        Ok(ScriptedSession {
                            index: 1,
                            log,
                            events: vec![
                                Ok(WorkerSessionEvent::Task(first_task)),
                                Ok(WorkerSessionEvent::Task(second_task)),
                            ],
                            fail_reports: true,
                            register_denial: None,
                            delay_stream: None,
                        })
                    } else if attempt == 2 {
                        Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: Vec::new(),
                            fail_reports: false,
                            register_denial: None,
                            delay_stream: None,
                        })
                    } else {
                        // A clean close no longer ends the run, so the third
                        // establishment is denied deterministically to end it.
                        Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: Vec::new(),
                            fail_reports: false,
                            register_denial: Some(tonic::Status::permission_denied(
                                "namespace `payments` revoked for subject `worker-a`",
                            )),
                            delay_stream: None,
                        })
                    }
                }
            }
        };

        let result = worker
            .run_with_connector_until(connect, std::future::pending::<()>())
            .await;

        drop(log_sender);
        let mut reports = Vec::new();
        while let Some(entry) = log_receiver.recv().await {
            if let SessionLog::Reported(index, report) = entry {
                reports.push((index, report));
            }
        }
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(!error.is_retryable());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(
            reports.len(),
            2,
            "both workflows' colliding sequence-position results must be re-reported"
        );
        let mut reported_workflows = Vec::new();
        for (session_index, report) in &reports {
            assert_eq!(*session_index, 2, "re-reports must land on the new session");
            let RecordedReport::Completed(reported_workflow, reported_id, _) = report else {
                return Err(WorkerError::decode(UnexpectedReportShape));
            };
            assert_eq!(reported_id, &activity_id);
            reported_workflows.push(reported_workflow.clone());
        }
        assert!(reported_workflows.contains(&first_workflow));
        assert!(reported_workflows.contains(&second_workflow));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_during_recovery_establishment_returns_original_drop_error()
    -> Result<(), WorkerError> {
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            let notify = Arc::clone(&notify);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let notify = Arc::clone(&notify);
                let log = log_sender.clone();
                async move {
                    if attempt == 1 {
                        Ok(ScriptedSession {
                            index: 1,
                            log,
                            events: vec![Err(WorkerError::Transport {
                                source: tonic::Status::unavailable("stream reset by peer"),
                            })],
                            fail_reports: false,
                            register_denial: None,
                            delay_stream: None,
                        })
                    } else {
                        // Fire shutdown while recovery is still inside the
                        // reconnect machinery's connect attempt, then hang
                        // so only the shutdown arm can win the select.
                        notify.notify_one();
                        std::future::pending::<()>().await;
                        Err(WorkerError::Transport {
                            source: tonic::Status::unavailable("unreachable"),
                        })
                    }
                }
            }
        };
        let shutdown = {
            let notify = Arc::clone(&notify);
            async move {
                notify.notified().await;
            }
        };

        let run = worker.run_with_connector_until(connect, shutdown);
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .map_err(WorkerError::decode)?;

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::Unavailable)
        ));
        assert_eq!(
            error.grpc_status().map(tonic::Status::message),
            Some("stream reset by peer"),
            "shutdown during recovery establishment must surface the original drop error"
        );
        drop(log_receiver);
        Ok(())
    }

    /// The paused clock keeps every session's lifetime at exactly zero, so
    /// no time-based budget reset can fire: flapping sessions that never
    /// serve a task must exhaust at exactly `max_attempts` drops.
    #[tokio::test(start_paused = true)]
    async fn mid_run_drop_budget_exhaustion_surfaces_last_drop_error() -> Result<(), WorkerError> {
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                async move {
                    Ok(ScriptedSession {
                        index: attempt,
                        log,
                        events: vec![Err(WorkerError::Transport {
                            source: tonic::Status::unavailable("stream reset by peer"),
                        })],
                        fail_reports: false,
                        register_denial: None,
                        delay_stream: None,
                    })
                }
            }
        };

        let run = worker.run_with_connector_until(connect, std::future::pending::<()>());
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .map_err(WorkerError::decode)?;

        // test_config allows 3 reconnect attempts; the third mid-run drop
        // exhausts the cumulative drop budget without another reconnect.
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::Unavailable)
        ));
        assert_eq!(
            error.grpc_status().map(tonic::Status::message),
            Some("stream reset by peer")
        );
        drop(log_receiver);
        Ok(())
    }

    #[tokio::test]
    async fn mid_run_denial_surfaces_without_reconnect() -> Result<(), WorkerError> {
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let log = log_sender.clone();
                async move {
                    Ok(ScriptedSession {
                        index: 1,
                        log,
                        events: vec![Err(WorkerError::Transport {
                            source: tonic::Status::permission_denied(
                                "namespace `payments` revoked for subject `worker-a`",
                            ),
                        })],
                        fail_reports: false,
                        register_denial: None,
                        delay_stream: None,
                    })
                }
            }
        };

        let result = worker
            .run_with_connector_until(connect, std::future::pending::<()>())
            .await;

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(!error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied)
        ));
        assert_eq!(
            error.grpc_status().map(tonic::Status::message),
            Some("namespace `payments` revoked for subject `worker-a`")
        );
        drop(log_receiver);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_during_establishment_backoff_returns_promptly() -> Result<(), WorkerError> {
        let worker = two_activity_worker_with(slow_reconnect_config())?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());
        let connect = {
            let attempts = Arc::clone(&attempts);
            let notify = Arc::clone(&notify);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                notify.notify_one();
                async move {
                    Err::<ScriptedSession, _>(WorkerError::Transport {
                        source: tonic::Status::unavailable("engine unreachable"),
                    })
                }
            }
        };
        let shutdown = {
            let notify = Arc::clone(&notify);
            async move {
                notify.notified().await;
            }
        };

        let run = worker.run_with_connector_until(connect, shutdown);
        tokio::time::timeout(Duration::from_millis(500), run)
            .await
            .map_err(WorkerError::decode)??;

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_during_mid_run_drop_backoff_returns_promptly() -> Result<(), WorkerError> {
        let worker = two_activity_worker_with(slow_reconnect_config())?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let log = log_sender.clone();
                async move {
                    Ok(ScriptedSession {
                        index: 1,
                        log,
                        events: vec![Err(WorkerError::Transport {
                            source: tonic::Status::unavailable("stream reset by peer"),
                        })],
                        fail_reports: false,
                        register_denial: None,
                        delay_stream: None,
                    })
                }
            }
        };
        let shutdown = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        let run = worker.run_with_connector_until(connect, shutdown);
        let result = tokio::time::timeout(Duration::from_millis(500), run)
            .await
            .map_err(WorkerError::decode)?;

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::Unavailable)
        ));
        drop(log_receiver);
        Ok(())
    }

    #[tokio::test]
    async fn served_tasks_reset_drop_budget_across_cycles() -> Result<(), WorkerError> {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(7);
        // max_backoff is enormous so only the served-task rule can reset the
        // budget; max_attempts = 2 so any two unhealthy drops would end the run.
        let worker = two_activity_worker_with(test_config_with(ReconnectConfig::new(
            Duration::from_millis(1),
            Duration::from_secs(3600),
            2,
        )))?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            let log_sender = log_sender.clone();
            let workflow_id = workflow_id.clone();
            let activity_id = activity_id.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                let task = proto_task(workflow_id.clone(), activity_id.clone(), "double", 21);
                async move {
                    if attempt <= 4 {
                        Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: vec![
                                Ok(WorkerSessionEvent::Task(task)),
                                Err(WorkerError::Transport {
                                    source: tonic::Status::unavailable("stream reset by peer"),
                                }),
                            ],
                            fail_reports: false,
                            register_denial: None,
                            delay_stream: None,
                        })
                    } else {
                        Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: Vec::new(),
                            fail_reports: false,
                            register_denial: Some(tonic::Status::permission_denied(
                                "namespace `payments` revoked for subject `worker-a`",
                            )),
                            delay_stream: None,
                        })
                    }
                }
            }
        };

        let run = worker.run_with_connector_until(connect, std::future::pending::<()>());
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .map_err(WorkerError::decode)?;

        drop(log_sender);
        let mut registrations = 0_usize;
        while let Some(entry) = log_receiver.recv().await {
            if let SessionLog::Registered(..) = entry {
                registrations += 1;
            }
        }
        // Four sessions each served a task before dropping; every served task
        // reset the cumulative budget (max_attempts = 2), so the worker kept
        // recovering well past the budget until the deterministic denial on
        // the fifth establishment ended the run fail-fast.
        assert_eq!(attempts.load(Ordering::SeqCst), 5);
        assert_eq!(registrations, 4);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(!error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied)
        ));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn session_outliving_max_backoff_resets_drop_budget() -> Result<(), WorkerError> {
        let worker = two_activity_worker_with(test_config_with(ReconnectConfig::new(
            Duration::from_millis(5),
            Duration::from_millis(20),
            2,
        )))?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                async move {
                    Ok(ScriptedSession {
                        index: attempt,
                        log,
                        events: vec![Err(WorkerError::Transport {
                            source: tonic::Status::unavailable("stream reset by peer"),
                        })],
                        fail_reports: false,
                        register_denial: None,
                        // Only the second session outlives the 20ms max
                        // backoff before dropping; the others drop instantly
                        // (the paused clock keeps their lifetimes at zero).
                        delay_stream: (attempt == 2).then_some(Duration::from_millis(30)),
                    })
                }
            }
        };

        let run = worker.run_with_connector_until(connect, std::future::pending::<()>());
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .map_err(WorkerError::decode)?;

        // Drop one consumed the first budget unit. The second session served
        // no tasks but survived past max_backoff, so its drop restarted the
        // count at one. The third session's instant drop was the second
        // post-reset unit and exhausted max_attempts = 2 — proving exactly
        // one unit was consumed before the reset. Without the reset the run
        // would have ended after two sessions.
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::Unavailable)
        ));
        drop(log_receiver);
        Ok(())
    }

    #[tokio::test]
    async fn clean_close_reconnects_re_registers_and_keeps_serving() -> Result<(), WorkerError> {
        let workflow_id = WorkflowId::new_v4();
        let first_activity = ActivityId::from_sequence_position(1);
        let second_activity = ActivityId::from_sequence_position(2);
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            let log_sender = log_sender.clone();
            let workflow_id = workflow_id.clone();
            let first_activity = first_activity.clone();
            let second_activity = second_activity.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                let first_task =
                    proto_task(workflow_id.clone(), first_activity.clone(), "double", 10);
                let second_task =
                    proto_task(workflow_id.clone(), second_activity.clone(), "double", 20);
                async move {
                    match attempt {
                        // Both sessions end with a clean server-side stream
                        // close after serving one task each.
                        1 => Ok(ScriptedSession {
                            index: 1,
                            log,
                            events: vec![Ok(WorkerSessionEvent::Task(first_task))],
                            fail_reports: false,
                            register_denial: None,
                            delay_stream: None,
                        }),
                        2 => Ok(ScriptedSession {
                            index: 2,
                            log,
                            events: vec![Ok(WorkerSessionEvent::Task(second_task))],
                            fail_reports: false,
                            register_denial: None,
                            delay_stream: None,
                        }),
                        _ => Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: Vec::new(),
                            fail_reports: false,
                            register_denial: Some(tonic::Status::permission_denied(
                                "namespace `payments` revoked for subject `worker-a`",
                            )),
                            delay_stream: None,
                        }),
                    }
                }
            }
        };

        let run = worker.run_with_connector_until(connect, std::future::pending::<()>());
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .map_err(WorkerError::decode)?;

        drop(log_sender);
        let mut registrations = Vec::new();
        let mut reports = Vec::new();
        while let Some(entry) = log_receiver.recv().await {
            match entry {
                SessionLog::Registered(index, types) => registrations.push((index, types)),
                SessionLog::Reported(index, report) => reports.push((index, report)),
            }
        }
        // Each clean close redialled through the budgeted cycle: the worker
        // re-registered, re-reported the unacknowledged backlog, and kept
        // serving until the deterministic denial ended the run.
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let expected_types = vec![String::from("double"), String::from("increment")];
        assert_eq!(
            registrations,
            vec![(1, expected_types.clone()), (2, expected_types)]
        );
        assert_eq!(reports.len(), 3);
        assert!(matches!(
            &reports[0],
            (1, RecordedReport::Completed(_, id, _)) if id == &first_activity
        ));
        assert!(matches!(
            &reports[1],
            (2, RecordedReport::Completed(_, id, _)) if id == &first_activity
        ));
        assert!(matches!(
            &reports[2],
            (2, RecordedReport::Completed(_, id, _)) if id == &second_activity
        ));
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(!error.is_retryable());
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied)
        ));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn clean_close_loop_exhausts_drop_budget_with_classified_error() -> Result<(), WorkerError>
    {
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                async move {
                    Ok(ScriptedSession {
                        index: attempt,
                        log,
                        events: Vec::new(),
                        fail_reports: false,
                        register_denial: None,
                        delay_stream: None,
                    })
                }
            }
        };

        let run = worker.run_with_connector_until(connect, std::future::pending::<()>());
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .map_err(WorkerError::decode)?;

        // test_config allows 3 attempts: with the paused clock no session
        // outlives max_backoff and none serves a task, so the third clean
        // close exhausts the budget with the classified clean-close error —
        // exactly the same accounting as error drops.
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(matches!(error, WorkerError::CleanCloseExhausted));
        assert!(error.to_string().contains("closed the stream cleanly"));
        drop(log_receiver);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_during_clean_close_backoff_returns_ok_promptly() -> Result<(), WorkerError> {
        let worker = two_activity_worker_with(slow_reconnect_config())?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let log = log_sender.clone();
                async move {
                    Ok(ScriptedSession {
                        index: 1,
                        log,
                        events: Vec::new(),
                        fail_reports: false,
                        register_denial: None,
                        delay_stream: None,
                    })
                }
            }
        };
        let shutdown = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        // The clean close enters the 5s drop backoff; shutdown must win it
        // promptly and a clean close pending recovery is not an error.
        let run = worker.run_with_connector_until(connect, shutdown);
        tokio::time::timeout(Duration::from_millis(500), run)
            .await
            .map_err(WorkerError::decode)??;

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        drop(log_receiver);
        Ok(())
    }
}
