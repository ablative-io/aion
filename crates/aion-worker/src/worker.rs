//! `Worker` builder, run loop, and shutdown wiring.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::activity::{ActivityRegistry, HandlerFuture};
use crate::config::WorkerConfig;
use crate::context::ActivityContext;
use crate::error::WorkerError;
use crate::protocol::reconnect::{connect_registered_grpc_session, register_connected_session};
use crate::protocol::{GrpcWorkerSession, WorkerSession};
use crate::runtime::{NoShutdown, serve_activity_tasks, serve_activity_tasks_until};

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

    /// Connects to the configured endpoint, registers activities, and serves until the stream ends.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] for connection, registration, dispatch, heartbeat, or report failures.
    pub async fn run(self) -> Result<(), WorkerError> {
        self.run_until(std::future::pending::<()>()).await
    }

    /// Connects to the configured endpoint, registers activities, and serves until shutdown fires.
    ///
    /// On shutdown, no new tasks are pulled, in-flight activity contexts are marked cancelled,
    /// and all in-flight activities are drained before this returns.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] for connection, registration, dispatch, heartbeat, or report failures.
    pub async fn run_until<Shutdown>(self, shutdown: Shutdown) -> Result<(), WorkerError>
    where
        Shutdown: Future<Output = ()> + Send,
    {
        let mut session = connect_registered_grpc_session(
            &self.config,
            self.activity_types.clone(),
            &self.available_handlers,
        )
        .await?;
        serve_activity_tasks_until(&self.config, &mut session, self.activities, shutdown).await
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
        serve_activity_tasks_until(&self.config, &mut session, self.activities, shutdown).await?;
        Ok(session)
    }
}

/// Connects and serves an already-built worker with the default non-shutdown future.
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
    use futures::stream;
    use serde::{Deserialize, Serialize};
    use tokio::sync::mpsc;

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
        Completed(ActivityId, Payload),
        Failed(ActivityId, ActivityError),
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
            let _ = workflow_id;
            self.reports
                .push(RecordedReport::Completed(activity_id, result));
            Ok(())
        }

        async fn report_failure(
            &mut self,
            workflow_id: WorkflowId,
            activity_id: ActivityId,
            failure: ActivityError,
        ) -> Result<(), WorkerError> {
            let _ = workflow_id;
            self.reports
                .push(RecordedReport::Failed(activity_id, failure));
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
            RecordedReport::Completed(reported_id, _) if reported_id == &activity_id
        ));
        Ok(())
    }

    fn two_activity_worker() -> Result<Worker, WorkerError> {
        Worker::builder(test_config())
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
    ) -> ProtoActivityTask {
        ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id)),
            activity_type: activity_type.to_owned(),
            input: Some(ProtoPayload::from(Payload::new(
                ContentType::Json,
                b"{\"value\":0}".to_vec(),
            ))),
        }
    }

    async fn wait_until_started(started: &AtomicUsize) {
        while started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    fn test_config() -> WorkerConfig {
        WorkerConfig::new(
            "http://127.0.0.1:50051",
            "payments",
            "worker-a",
            1,
            ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(20), 3),
            None,
        )
    }

    #[derive(Debug, thiserror::Error)]
    #[error("failed to send shutdown signal")]
    struct SendFailed;
}
