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
use crate::runtime::{
    NoShutdown, ServeEnd, SessionHealth, serve_activity_tasks, serve_activity_tasks_until,
};

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

    /// Announce an established session: operators watching the worker's logs
    /// previously got no positive signal that registration succeeded (only
    /// drop/backoff warnings on failure).
    fn log_session_established(&self) {
        info!(
            identity = %self.config.identity,
            endpoint = %self.config.endpoint,
            activity_types = ?self.activity_types,
            "worker session established; serving activities"
        );
    }

    /// Connects to the configured endpoint, registers activities, and serves indefinitely.
    ///
    /// Registration completes only when the server's `RegisterAck` — the
    /// guaranteed first response frame — arrives; the worker serves nothing
    /// before it. Session establishment goes through the bounded-backoff
    /// reconnect machinery configured in [`WorkerConfig::reconnect`], and
    /// retryable mid-run transport drops — including clean server-side
    /// stream closes — re-establish through the same machinery: the worker
    /// re-registers its activity types, re-reports every unacknowledged
    /// activity result (cleared only by the server's per-result `ResultAck`
    /// frames), and resumes serving. A server-announced drain reconnects
    /// after the schedule's initial backoff without consuming drop budget.
    /// Deterministic `PermissionDenied` / `Unauthenticated` denials surface
    /// after exactly one attempt. Without a shutdown signal the run ends
    /// only on a non-retryable error or drop-budget exhaustion; see
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
    /// failure or an unannounced clean server-side stream close, both count —
    /// the worker drains in-flight activities into the unacked tracker, backs
    /// off, reconnects through the same machinery (re-registering its
    /// activity types), re-reports every still-unacknowledged result (the
    /// shutdown signal can interrupt that replay; tracked results survive),
    /// and resumes serving. Server `ResultAck` frames clear tracker entries
    /// mid-session, so the steady-state replay backlog is empty.
    ///
    /// Mid-run drops share one cumulative budget of `reconnect.max_attempts`,
    /// matching the Python and TypeScript workers, and the budget resets to
    /// zero once a session proves healthy: it served at least one task, or it
    /// stayed connected longer than `reconnect.max_backoff` (measured
    /// monotonically from successful registration to the moment the stream
    /// ended or dropped — post-drop draining of in-flight activities never
    /// extends it). A server-announced drain is unbudgeted: the worker
    /// finishes in-flight work and redials after `reconnect.initial_backoff`;
    /// the drain classification latches for the session, so even an abrupt
    /// end after the drain frame stays drain-class. See
    /// [`crate::config::ReconnectConfig`]. The run therefore ends only on
    /// shutdown, a non-retryable error, or budget exhaustion — never merely
    /// because the server closed or drained the stream. At most one session
    /// is alive at a time, and a shutdown signalled during a reconnect or
    /// backoff wins promptly (returning `Ok` when the pending drop was a
    /// drain or clean close, and the pending error when it was a failure).
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
            self.log_session_established();
            let session_started = tokio::time::Instant::now();
            let mut health = SessionHealth::default();
            // The unacked-result replay races shutdown so a hung re-report
            // send can never wedge worker shutdown. Results stay tracked —
            // entries are recorded before any send and only an explicit ack
            // removes them — so nothing is lost by abandoning the replay.
            let replay = tokio::select! {
                biased;
                () = shutdown.wait() => None,
                result = re_report_unacked(&tracker, &mut session) => Some(result),
            };
            let Some(replay_result) = replay else {
                return Ok(());
            };
            let served = match replay_result {
                Ok(()) => {
                    serve_activity_tasks_until(
                        &self.config,
                        &mut session,
                        Arc::clone(&self.activities),
                        &mut tracker,
                        &mut health,
                        shutdown.wait(),
                    )
                    .await
                }
                Err(report_error) => Err(report_error),
            };
            drop(session);
            let cause = match classify_serve_outcome(served, &health, shutdown.fired()) {
                ServeClassification::End(result) => return result,
                ServeClassification::Drop(cause) => cause,
            };
            // Connected lifetime is measured from successful registration to
            // the moment the stream ended — never to the end of the post-drop
            // drain, which would let a long-running in-flight handler
            // masquerade as a healthy session and reset the budget forever.
            // A replay failure never enters the serve loop, so its drop
            // moment is now.
            let connected_for = health
                .stream_ended_at
                .unwrap_or_else(tokio::time::Instant::now)
                .saturating_duration_since(session_started);
            let proved_healthy = health.tasks_reported > 0 || connected_for > backoff.max_delay();
            if proved_healthy && drop_failures > 0 {
                info!(
                    drop_failures,
                    tasks_reported = health.tasks_reported,
                    "worker session proved healthy; drop budget reset"
                );
                drop_failures = 0;
            }
            // An announced drain consumes no drop budget: the server told the
            // worker it was going away, so the drop is expected operator
            // behaviour, not flapping. Unannounced closes and failures stay
            // budgeted exactly as before.
            let delay = if matches!(cause, DropCause::Drain) {
                self.config.reconnect.initial_backoff
            } else {
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
                backoff.delay_for_attempt(drop_failures)
            };
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
        let mut health = SessionHealth::default();
        serve_activity_tasks_until(
            &self.config,
            &mut session,
            self.activities,
            &mut tracker,
            &mut health,
            shutdown,
        )
        .await?;
        Ok(session)
    }
}

/// What the run loop does with a finished session: end the run or recover.
enum ServeClassification {
    /// The run ends now with this result.
    End(Result<(), WorkerError>),
    /// The session dropped retryably; enter the recovery cycle.
    Drop(DropCause),
}

/// Classifies a serve outcome per the cross-SDK contract: shutdown ends the
/// run (a pending drain or clean close cleanly, a pending error-class drop
/// with its error), denials are terminal, a drain — announced by the frame
/// even when the stream later ended abruptly (the latch in
/// [`SessionHealth::drain_received`]) — is an unbudgeted drop, and
/// everything else is a budgeted retryable drop.
fn classify_serve_outcome(
    served: Result<ServeEnd, WorkerError>,
    health: &SessionHealth,
    shutdown_fired: bool,
) -> ServeClassification {
    match served {
        Ok(ServeEnd::Shutdown) => ServeClassification::End(Ok(())),
        Ok(ServeEnd::Drained) => {
            if shutdown_fired {
                return ServeClassification::End(Ok(()));
            }
            ServeClassification::Drop(DropCause::Drain)
        }
        Ok(ServeEnd::StreamClosed) => {
            if shutdown_fired {
                return ServeClassification::End(Ok(()));
            }
            ServeClassification::Drop(DropCause::CleanClose)
        }
        Err(error) if !error.is_retryable() => {
            error!(error = %error, "worker session denied by server; not reconnecting");
            ServeClassification::End(Err(error))
        }
        Err(error) if health.drain_received => {
            // Drain latch: the server announced it was going away, so the
            // abrupt end (or a failed post-drain report) is drain-class.
            // Surface the suppressed error loudly.
            warn!(
                error = %error,
                "session error after server drain; classified as drain drop"
            );
            if shutdown_fired {
                return ServeClassification::End(Ok(()));
            }
            ServeClassification::Drop(DropCause::Drain)
        }
        Err(error) => {
            if shutdown_fired {
                return ServeClassification::End(Err(error));
            }
            ServeClassification::Drop(DropCause::Failure(error))
        }
    }
}

/// Cause of a retryable mid-run session drop carried across one recovery cycle.
enum DropCause {
    /// The session ended with a retryable error.
    Failure(WorkerError),
    /// The server closed the stream cleanly without announcing a drain.
    CleanClose,
    /// The server announced a drain before the session ended. Unbudgeted:
    /// the redial happens after the schedule's initial backoff.
    Drain,
}

impl DropCause {
    /// The classified error surfaced when this drop exhausts the budget.
    ///
    /// `Drain` never consumes budget, so it cannot exhaust it; the mapping
    /// exists only for match exhaustiveness and mirrors the clean-close
    /// classification (a drain is an announced clean close).
    fn into_exhaustion_error(self) -> WorkerError {
        match self {
            Self::Failure(error) => error,
            Self::CleanClose | Self::Drain => WorkerError::CleanCloseExhausted,
        }
    }

    /// Run outcome when shutdown wins the post-drop backoff: an error drop
    /// surfaces its error, a drain or clean close is a graceful end.
    fn into_shutdown_result(self) -> Result<(), WorkerError> {
        match self {
            Self::Failure(error) => Err(error),
            Self::CleanClose | Self::Drain => Ok(()),
        }
    }

    /// Error to surface if shutdown wins the recovery establishment select.
    fn into_recovery_error(self) -> Option<WorkerError> {
        match self {
            Self::Failure(error) => Some(error),
            Self::CleanClose | Self::Drain => None,
        }
    }
}

impl std::fmt::Display for DropCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failure(error) => write!(formatter, "{error}"),
            Self::CleanClose => write!(formatter, "server closed the worker stream cleanly"),
            Self::Drain => write!(formatter, "server drained the worker stream"),
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

    /// Session whose reports hang forever, modelling a server that
    /// stopped reading its inbound stream during the replay.
    struct HungReportSession {
        log: mpsc::UnboundedSender<SessionLog>,
        index: usize,
    }

    #[async_trait]
    impl WorkerSession for HungReportSession {
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
            self.log
                .send(SessionLog::Registered(self.index, activity_types))
                .map_err(WorkerError::decode)
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            Box::pin(stream::pending())
        }

        async fn report_result(
            &mut self,
            _workflow_id: WorkflowId,
            _activity_id: ActivityId,
            _result: Payload,
        ) -> Result<(), WorkerError> {
            std::future::pending::<()>().await;
            Ok(())
        }

        async fn report_failure(
            &mut self,
            _workflow_id: WorkflowId,
            _activity_id: ActivityId,
            _failure: ActivityError,
        ) -> Result<(), WorkerError> {
            std::future::pending::<()>().await;
            Ok(())
        }

        async fn send_heartbeat(
            &mut self,
            _workflow_id: WorkflowId,
            _activity_id: ActivityId,
            _progress: Option<Payload>,
        ) -> Result<(), WorkerError> {
            Ok(())
        }
    }

    enum SessionKind {
        Scripted(ScriptedSession),
        Hung(HungReportSession),
    }

    #[async_trait]
    impl WorkerSession for SessionKind {
        async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
            match self {
                Self::Scripted(session) => session.handshake(config).await,
                Self::Hung(session) => session.handshake(config).await,
            }
        }

        async fn register(
            &mut self,
            activity_types: Vec<String>,
            available_handlers: &BTreeSet<String>,
        ) -> Result<(), WorkerError> {
            match self {
                Self::Scripted(session) => {
                    session.register(activity_types, available_handlers).await
                }
                Self::Hung(session) => session.register(activity_types, available_handlers).await,
            }
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            match self {
                Self::Scripted(session) => session.receive_tasks(),
                Self::Hung(session) => session.receive_tasks(),
            }
        }

        async fn report_result(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            result: Payload,
        ) -> Result<(), WorkerError> {
            match self {
                Self::Scripted(session) => {
                    session
                        .report_result(workflow_id, activity_id, result)
                        .await
                }
                Self::Hung(session) => {
                    session
                        .report_result(workflow_id, activity_id, result)
                        .await
                }
            }
        }

        async fn report_failure(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            failure: ActivityError,
        ) -> Result<(), WorkerError> {
            match self {
                Self::Scripted(session) => {
                    session
                        .report_failure(workflow_id, activity_id, failure)
                        .await
                }
                Self::Hung(session) => {
                    session
                        .report_failure(workflow_id, activity_id, failure)
                        .await
                }
            }
        }

        async fn send_heartbeat(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            progress: Option<Payload>,
        ) -> Result<(), WorkerError> {
            match self {
                Self::Scripted(session) => {
                    session
                        .send_heartbeat(workflow_id, activity_id, progress)
                        .await
                }
                Self::Hung(session) => {
                    session
                        .send_heartbeat(workflow_id, activity_id, progress)
                        .await
                }
            }
        }
    }

    /// Session that emits one task + drain and fails exactly the report
    /// for `fail_id`, modelling a server that crashed after announcing
    /// its drain; re-reports of earlier sessions' entries succeed.
    struct DrainLatchSession {
        events: Vec<Result<WorkerSessionEvent, WorkerError>>,
        fail_id: ActivityId,
    }

    #[async_trait]
    impl WorkerSession for DrainLatchSession {
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
            Box::pin(stream::iter(std::mem::take(&mut self.events)))
        }

        async fn report_result(
            &mut self,
            _workflow_id: WorkflowId,
            activity_id: ActivityId,
            _result: Payload,
        ) -> Result<(), WorkerError> {
            if activity_id == self.fail_id {
                return Err(WorkerError::Transport {
                    source: tonic::Status::unavailable(
                        "stream broke abruptly after the drain frame",
                    ),
                });
            }
            Ok(())
        }

        async fn report_failure(
            &mut self,
            _workflow_id: WorkflowId,
            _activity_id: ActivityId,
            _failure: ActivityError,
        ) -> Result<(), WorkerError> {
            Ok(())
        }

        async fn send_heartbeat(
            &mut self,
            _workflow_id: WorkflowId,
            _activity_id: ActivityId,
            _progress: Option<Payload>,
        ) -> Result<(), WorkerError> {
            Ok(())
        }
    }

    enum LatchKind {
        Latch(DrainLatchSession),
        Deny(ScriptedSession),
    }

    #[async_trait]
    impl WorkerSession for LatchKind {
        async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
            match self {
                Self::Latch(session) => session.handshake(config).await,
                Self::Deny(session) => session.handshake(config).await,
            }
        }

        async fn register(
            &mut self,
            activity_types: Vec<String>,
            available_handlers: &BTreeSet<String>,
        ) -> Result<(), WorkerError> {
            match self {
                Self::Latch(session) => session.register(activity_types, available_handlers).await,
                Self::Deny(session) => session.register(activity_types, available_handlers).await,
            }
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            match self {
                Self::Latch(session) => session.receive_tasks(),
                Self::Deny(session) => session.receive_tasks(),
            }
        }

        async fn report_result(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            result: Payload,
        ) -> Result<(), WorkerError> {
            match self {
                Self::Latch(session) => {
                    session
                        .report_result(workflow_id, activity_id, result)
                        .await
                }
                Self::Deny(session) => {
                    session
                        .report_result(workflow_id, activity_id, result)
                        .await
                }
            }
        }

        async fn report_failure(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            failure: ActivityError,
        ) -> Result<(), WorkerError> {
            match self {
                Self::Latch(session) => {
                    session
                        .report_failure(workflow_id, activity_id, failure)
                        .await
                }
                Self::Deny(session) => {
                    session
                        .report_failure(workflow_id, activity_id, failure)
                        .await
                }
            }
        }

        async fn send_heartbeat(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            progress: Option<Payload>,
        ) -> Result<(), WorkerError> {
            match self {
                Self::Latch(session) => {
                    session
                        .send_heartbeat(workflow_id, activity_id, progress)
                        .await
                }
                Self::Deny(session) => {
                    session
                        .send_heartbeat(workflow_id, activity_id, progress)
                        .await
                }
            }
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
            attempt: 1,
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

    /// Connected lifetime is measured to the stream end, not to the end of
    /// the post-drop drain: a 60ms in-flight handler draining past the 20ms
    /// max backoff after the stream already dropped (with its report failing,
    /// so no task counts as served) must not reset the budget. Measured to
    /// the end of the drain, every cycle would reset the budget and the
    /// worker would flap forever instead of exhausting.
    #[tokio::test(start_paused = true)]
    async fn post_drop_drain_time_does_not_reset_drop_budget() -> Result<(), WorkerError> {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(9);
        // max_concurrency = 2 so the stream error is read while the slow
        // handler still holds the first dispatch permit.
        let config = WorkerConfig::new(
            "http://127.0.0.1:50051",
            "payments",
            "worker-a",
            2,
            ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(20), 2),
            None,
        );
        let worker = Worker::builder(config)
            .register_activity("slow", |input: TestInput, context: &ActivityContext| {
                let _ = (input, context);
                Box::pin(async move {
                    // Outlives the 20ms max backoff on the paused clock while
                    // the post-drop drain awaits this handler.
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    Ok(TestOutput { value: 1 })
                })
            })?
            .build()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            let workflow_id = workflow_id.clone();
            let activity_id = activity_id.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                let task = proto_task(workflow_id.clone(), activity_id.clone(), "slow", 1);
                async move {
                    if attempt == 1 {
                        // Instant drop with no task: consumes the first
                        // budget unit and leaves the unacked tracker empty,
                        // so the second cycle reaches its serve loop.
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
                        // The server dispatches the 60ms task and kills the
                        // stream immediately. Failed reports keep
                        // tasks_reported at zero, so only a (mis)measured
                        // connected lifetime could reset the budget.
                        Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: vec![
                                Ok(WorkerSessionEvent::Task(task)),
                                Err(WorkerError::Transport {
                                    source: tonic::Status::unavailable("stream reset by peer"),
                                }),
                            ],
                            fail_reports: true,
                            register_denial: None,
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

        // The second session's stream dropped at a connected lifetime of ~0
        // on the paused clock while its 60ms handler drained past the 20ms
        // max backoff; it never proved healthy, so its drop exhausted
        // max_attempts = 2. Measured to the end of the drain instead, the
        // second cycle would have reset the budget and dialled a third
        // session.
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
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

    /// Brief test 13: a `ResultAck` event clears exactly its tracker entry —
    /// two workflows colliding on the bare sequence position exercise both
    /// key components — and an unknown ack is a no-op.
    #[tokio::test]
    async fn result_ack_clears_exactly_its_tracker_entry() -> Result<(), WorkerError> {
        use crate::protocol::reconnect::{PendingActivityReport, UnackedResultTracker};
        use crate::runtime::loop_::{SessionHealth, serve_activity_tasks_until};

        let workflow_a = WorkflowId::new_v4();
        let workflow_b = WorkflowId::new_v4();
        let position = ActivityId::from_sequence_position(5);
        let mut tracker = UnackedResultTracker::new();
        for workflow in [&workflow_a, &workflow_b] {
            tracker.record(PendingActivityReport::Completed {
                workflow_id: workflow.clone(),
                activity_id: position.clone(),
                output: Payload::new(ContentType::Json, b"{\"value\":1}".to_vec()),
            });
        }

        let worker = two_activity_worker()?;
        let mut session = ChannelSession {
            receiver: None,
            reports: Vec::new(),
            registered: Vec::new(),
        };
        let (sender, receiver) = mpsc::channel(4);
        sender
            .send(Ok(WorkerSessionEvent::ResultAck {
                workflow_id: workflow_a.clone(),
                activity_id: position.clone(),
            }))
            .await
            .map_err(WorkerError::decode)?;
        // Unknown ack: never recorded; must be a no-op, not an error.
        sender
            .send(Ok(WorkerSessionEvent::ResultAck {
                workflow_id: WorkflowId::new_v4(),
                activity_id: ActivityId::from_sequence_position(99),
            }))
            .await
            .map_err(WorkerError::decode)?;
        drop(sender);
        session.receiver = Some(receiver);

        let mut health = SessionHealth::default();
        serve_activity_tasks_until(
            &test_config(),
            &mut session,
            Arc::new(crate::activity::ActivityRegistry::new()),
            &mut tracker,
            &mut health,
            std::future::pending(),
        )
        .await?;

        assert_eq!(tracker.len(), 1, "exactly the acked entry must clear");
        assert!(tracker.get(&workflow_a, &position).is_none());
        assert!(tracker.get(&workflow_b, &position).is_some());
        drop(worker);
        Ok(())
    }

    /// Brief tests 14 + 15: acks drain the tracker mid-session so the
    /// next-session replay sends nothing (steady-state decay); a lost ack
    /// costs exactly one re-report, cleared by the next session's ack.
    #[tokio::test]
    async fn acked_results_decay_out_of_the_reconnect_replay() -> Result<(), WorkerError> {
        use crate::protocol::re_report_unacked;
        use crate::protocol::reconnect::{PendingActivityReport, UnackedResultTracker};
        use crate::runtime::loop_::{SessionHealth, serve_activity_tasks_until};

        let workflow_id = WorkflowId::new_v4();
        let acked_id = ActivityId::from_sequence_position(1);
        let unacked_id = ActivityId::from_sequence_position(2);
        let mut tracker = UnackedResultTracker::new();
        for id in [&acked_id, &unacked_id] {
            tracker.record(PendingActivityReport::Completed {
                workflow_id: workflow_id.clone(),
                activity_id: id.clone(),
                output: Payload::new(ContentType::Json, b"{\"value\":2}".to_vec()),
            });
        }

        // Session 1 acks one of the two reported results; the other ack is
        // "lost" (never sent).
        let mut session = ChannelSession {
            receiver: None,
            reports: Vec::new(),
            registered: Vec::new(),
        };
        let (sender, receiver) = mpsc::channel(2);
        sender
            .send(Ok(WorkerSessionEvent::ResultAck {
                workflow_id: workflow_id.clone(),
                activity_id: acked_id.clone(),
            }))
            .await
            .map_err(WorkerError::decode)?;
        drop(sender);
        session.receiver = Some(receiver);
        let mut health = SessionHealth::default();
        serve_activity_tasks_until(
            &test_config(),
            &mut session,
            Arc::new(crate::activity::ActivityRegistry::new()),
            &mut tracker,
            &mut health,
            std::future::pending(),
        )
        .await?;

        // Session 2 replay: exactly the un-acked entry is re-reported.
        let mut replay_session = ChannelSession {
            receiver: None,
            reports: Vec::new(),
            registered: Vec::new(),
        };
        re_report_unacked(&tracker, &mut replay_session).await?;
        assert_eq!(
            replay_session.reports.len(),
            1,
            "only the un-acked result may be re-reported"
        );
        assert!(matches!(
            &replay_session.reports[0],
            RecordedReport::Completed(_, id, _) if id == &unacked_id
        ));

        // Session 2 acks the re-report; the tracker is now empty and a third
        // session's replay sends nothing.
        let (sender, receiver) = mpsc::channel(2);
        sender
            .send(Ok(WorkerSessionEvent::ResultAck {
                workflow_id: workflow_id.clone(),
                activity_id: unacked_id.clone(),
            }))
            .await
            .map_err(WorkerError::decode)?;
        drop(sender);
        replay_session.receiver = Some(receiver);
        let mut health = SessionHealth::default();
        serve_activity_tasks_until(
            &test_config(),
            &mut replay_session,
            Arc::new(crate::activity::ActivityRegistry::new()),
            &mut tracker,
            &mut health,
            std::future::pending(),
        )
        .await?;
        assert!(tracker.is_empty(), "acks must drain the tracker");

        let mut decayed_session = ChannelSession {
            receiver: None,
            reports: Vec::new(),
            registered: Vec::new(),
        };
        re_report_unacked(&tracker, &mut decayed_session).await?;
        assert!(
            decayed_session.reports.is_empty(),
            "steady-state replay must send nothing"
        );
        Ok(())
    }

    /// Brief test 17: shutdown interrupts a hung `re_report_unacked` send
    /// promptly instead of waiting it out; the hung session reports nothing.
    #[tokio::test(start_paused = true)]
    async fn shutdown_interrupts_hung_unacked_replay_promptly() -> Result<(), WorkerError> {
        // Two-faced connector: session 1 serves one task whose report send
        // fails (seeding the unacked tracker), session 2 hangs its replay.
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(3);
        let worker = two_activity_worker()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel();
        let (registered_2_tx, registered_2_rx) = tokio::sync::oneshot::channel::<()>();
        let registered_2_tx = std::sync::Mutex::new(Some(registered_2_tx));
        let connect = {
            let log_sender = log_sender.clone();
            let workflow_id = workflow_id.clone();
            let activity_id = activity_id.clone();
            move |attempt_override: usize| {
                let log = log_sender.clone();
                let task = proto_task(workflow_id.clone(), activity_id.clone(), "double", 21);
                let notify = if attempt_override == 2 {
                    registered_2_tx
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take())
                } else {
                    None
                };
                async move {
                    if attempt_override == 1 {
                        Ok(SessionKind::Scripted(ScriptedSession {
                            index: 1,
                            log,
                            events: vec![Ok(WorkerSessionEvent::Task(task))],
                            fail_reports: true,
                            register_denial: None,
                            delay_stream: None,
                        }))
                    } else {
                        if let Some(notify) = notify {
                            let _ = notify.send(());
                        }
                        Ok(SessionKind::Hung(HungReportSession { index: 2, log }))
                    }
                }
            }
        };

        let attempts_for_connect = Arc::clone(&attempts);
        let run = worker.run_with_connector_until(
            move || {
                let attempt = attempts_for_connect.fetch_add(1, Ordering::SeqCst) + 1;
                connect(attempt)
            },
            async move {
                let _ = registered_2_rx.await;
            },
        );

        // The hung session's replay never resolves; the session-2 oneshot
        // fires shutdown, which must win promptly.
        tokio::time::timeout(Duration::from_secs(60), run)
            .await
            .map_err(WorkerError::decode)??;

        drop(log_sender);
        let mut hung_session_reports = 0_usize;
        while let Some(entry) = log_receiver.recv().await {
            if let SessionLog::Reported(2, _) = entry {
                hung_session_reports += 1;
            }
        }
        assert_eq!(
            hung_session_reports, 0,
            "the hung replay must not have produced a report"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        Ok(())
    }

    /// Brief test 18: a server-announced drain consumes no drop budget —
    /// with a budget of two, three drain cycles still leave the worker
    /// running; a deterministic denial then ends the run.
    #[tokio::test(start_paused = true)]
    async fn drain_cycles_reconnect_without_consuming_drop_budget() -> Result<(), WorkerError> {
        let worker = two_activity_worker_with(test_config_with(ReconnectConfig::new(
            Duration::from_millis(5),
            Duration::from_millis(20),
            2,
        )))?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, mut log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                async move {
                    if attempt <= 3 {
                        Ok(ScriptedSession {
                            index: attempt,
                            log,
                            events: vec![Ok(WorkerSessionEvent::Drain)],
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

        let result = worker
            .run_with_connector_until(connect, std::future::pending::<()>())
            .await;

        // Three drain cycles with max_attempts = 2: if drains consumed
        // budget the run would have ended with CleanCloseExhausted after
        // the second; instead it survives to the scripted denial.
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied)
        ));
        let mut registrations = 0_usize;
        while let Some(entry) = log_receiver.recv().await {
            if matches!(entry, SessionLog::Registered(..)) {
                registrations += 1;
            }
        }
        assert_eq!(registrations, 3, "every drain cycle must re-register");
        Ok(())
    }

    /// Brief test 19: the drain classification latches — a session whose
    /// post-drain report fails abruptly is still drain-class and unbudgeted.
    /// Replay sends of older entries succeed (a replay failure is an
    /// *unannounced* drop and stays budgeted, per the reconnect record), so
    /// each session fails only its own task's report — after the drain frame.
    #[tokio::test(start_paused = true)]
    async fn drain_latch_keeps_abrupt_post_drain_failures_unbudgeted() -> Result<(), WorkerError> {
        let workflow_id = WorkflowId::new_v4();
        // The activity sleeps on the paused clock so its outcome can only be
        // reported once the serve loop has gone idle — i.e. after the drain
        // frame has been read and the loop is draining in-flight work. The
        // failing report is therefore deterministically post-drain.
        let worker = Worker::builder(test_config_with(ReconnectConfig::new(
            Duration::from_millis(5),
            Duration::from_millis(20),
            2,
        )))
        .register_activity("slow_double", |input: TestInput, context| {
            Box::pin(async move {
                let _ = context;
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok(TestOutput {
                    value: input.value * 2,
                })
            })
        })?
        .build()?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let connect = {
            let attempts = Arc::clone(&attempts);
            let workflow_id = workflow_id.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let log = log_sender.clone();
                let attempt_u64 = u64::try_from(attempt).unwrap_or(u64::MAX);
                let activity_id = ActivityId::from_sequence_position(attempt_u64);
                let task = proto_task(workflow_id.clone(), activity_id.clone(), "slow_double", 21);
                async move {
                    if attempt <= 3 {
                        Ok(LatchKind::Latch(DrainLatchSession {
                            events: vec![
                                Ok(WorkerSessionEvent::Task(task)),
                                Ok(WorkerSessionEvent::Drain),
                            ],
                            fail_id: activity_id,
                        }))
                    } else {
                        Ok(LatchKind::Deny(ScriptedSession {
                            index: attempt,
                            log,
                            events: Vec::new(),
                            fail_reports: false,
                            register_denial: Some(tonic::Status::permission_denied(
                                "namespace `payments` revoked for subject `worker-a`",
                            )),
                            delay_stream: None,
                        }))
                    }
                }
            }
        };

        let result = worker
            .run_with_connector_until(connect, std::future::pending::<()>())
            .await;

        // Three latched drain-class failures with max_attempts = 2: only the
        // latch keeps the run alive to the scripted denial.
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
        let Err(error) = result else {
            return Err(WorkerError::decode(UnexpectedSuccess));
        };
        assert!(matches!(
            error.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied)
        ));
        drop(log_receiver);
        Ok(())
    }

    /// Brief test 21: shutdown during the post-drain redial backoff ends the
    /// run `Ok` — a pending drain is not an error (the error-class
    /// counterpart is pinned by `shutdown_during_mid_run_drop_backoff_*`).
    #[tokio::test]
    async fn shutdown_during_post_drain_backoff_returns_ok_promptly() -> Result<(), WorkerError> {
        let worker = two_activity_worker_with(test_config_with(ReconnectConfig::new(
            Duration::from_secs(5),
            Duration::from_secs(10),
            5,
        )))?;
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
                        events: vec![Ok(WorkerSessionEvent::Drain)],
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

        // The drain enters the 5s initial-backoff redial sleep; shutdown
        // must win it promptly and a pending drain is a graceful end.
        let run = worker.run_with_connector_until(connect, shutdown);
        tokio::time::timeout(Duration::from_millis(500), run)
            .await
            .map_err(WorkerError::decode)??;

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        drop(log_receiver);
        Ok(())
    }
}
