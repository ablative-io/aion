//! handler invocation, payload decode/encode, failure classification

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;

use aion_core::{ActivityError, Payload};
use async_trait::async_trait;
use futures::FutureExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::error;

use crate::activity::ActivityFailure;
use crate::context::ActivityContext;
use crate::error::{MissingActivityHandler, WorkerError};
use crate::protocol::ActivityTask;
use crate::runtime::loop_::{ActivityDispatcher, DispatchOutcome};

/// Boxed future returned by a typed activity handler.
pub type HandlerFuture<'context, Output> =
    Pin<Box<dyn Future<Output = Result<Output, ActivityFailure>> + Send + 'context>>;

type BoxedHandler<Input, Output> = Box<
    dyn for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
        + Send
        + Sync,
>;

/// Minimal typed dispatcher registry for executing activity handlers.
#[derive(Default)]
pub struct TypedActivityDispatcher {
    handlers: BTreeMap<String, Box<dyn ErasedActivityHandler>>,
}

impl TypedActivityDispatcher {
    /// Creates an empty typed dispatcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one typed activity handler under an activity type name.
    #[must_use]
    pub fn register<Input, Output, Handler>(
        mut self,
        activity_type: impl Into<String>,
        handler: Handler,
    ) -> Self
    where
        Input: DeserializeOwned + Send + Sync + 'static,
        Output: Serialize + Send + Sync + 'static,
        Handler: for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
            + Send
            + Sync
            + 'static,
    {
        self.handlers
            .insert(activity_type.into(), Box::new(TypedHandler::new(handler)));
        self
    }
}

#[async_trait]
impl ActivityDispatcher for TypedActivityDispatcher {
    async fn dispatch(&self, task: ActivityTask) -> Result<DispatchOutcome, WorkerError> {
        let Some(handler) = self.handlers.get(&task.activity_type) else {
            return Err(WorkerError::registration(MissingActivityHandler {
                activity_type: task.activity_type,
            }));
        };
        handler.dispatch(task).await
    }

    fn activity_types(&self) -> BTreeSet<String> {
        self.handlers.keys().cloned().collect()
    }
}

/// Decodes a payload into a typed value using the payload content-type tag.
///
/// # Errors
///
/// Returns [`WorkerError::Decode`] when the payload tag or bytes cannot produce
/// the requested type.
pub fn decode_payload<T>(payload: &Payload) -> Result<T, WorkerError>
where
    T: DeserializeOwned,
{
    let value = payload.to_json().map_err(WorkerError::decode)?;
    serde_json::from_value(value).map_err(WorkerError::decode)
}

/// Encodes a typed value into the baseline JSON payload codec.
///
/// # Errors
///
/// Returns [`WorkerError::Encode`] when the value cannot be serialized.
pub fn encode_payload<T>(value: &T) -> Result<Payload, WorkerError>
where
    T: Serialize,
{
    let value = serde_json::to_value(value).map_err(WorkerError::encode)?;
    Payload::from_json(&value).map_err(WorkerError::encode)
}

#[async_trait]
trait ErasedActivityHandler: Send + Sync {
    async fn dispatch(&self, task: ActivityTask) -> Result<DispatchOutcome, WorkerError>;
}

struct TypedHandler<Input, Output> {
    handler: BoxedHandler<Input, Output>,
}

impl<Input, Output> TypedHandler<Input, Output> {
    fn new(
        handler: impl for<'context> Fn(
            Input,
            &'context ActivityContext,
        ) -> HandlerFuture<'context, Output>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }
}

#[async_trait]
impl<Input, Output> ErasedActivityHandler for TypedHandler<Input, Output>
where
    Input: DeserializeOwned + Send + Sync + 'static,
    Output: Serialize + Send + Sync + 'static,
{
    async fn dispatch(&self, task: ActivityTask) -> Result<DispatchOutcome, WorkerError> {
        let input = match decode_payload::<Input>(&task.input) {
            Ok(input) => input,
            Err(error) => {
                error!(
                    activity_type = %task.activity_type,
                    activity_id = task.activity_id.sequence_position(),
                    attempt = task.attempt,
                    error = %error,
                    "failed to decode activity input; reporting terminal activity failure"
                );
                let failure =
                    ActivityFailure::terminal(format!("failed to decode activity input: {error}"));
                return Ok(DispatchOutcome::Failed {
                    failure: ActivityError::from(failure),
                });
            }
        };
        let (context, cancellation_handle) =
            ActivityContext::new(task.activity_id.clone(), task.attempt);
        drop(cancellation_handle);
        let handler_future =
            match std::panic::catch_unwind(AssertUnwindSafe(|| (self.handler)(input, &context))) {
                Ok(handler_future) => handler_future,
                Err(panic) => return Ok(panic_failure(&task, &panic)),
            };
        let handler_result = AssertUnwindSafe(handler_future).catch_unwind().await;
        match handler_result {
            Ok(Ok(output)) => Ok(DispatchOutcome::Completed {
                output: encode_payload(&output)?,
            }),
            Ok(Err(failure)) => Ok(DispatchOutcome::Failed {
                failure: ActivityError::from(failure),
            }),
            Err(panic) => Ok(panic_failure(&task, &panic)),
        }
    }
}

fn panic_failure(task: &ActivityTask, panic: &Box<dyn Any + Send>) -> DispatchOutcome {
    let message = panic_message(panic);
    error!(
        activity_type = %task.activity_type,
        activity_id = task.activity_id.sequence_position(),
        attempt = task.attempt,
        panic = %message,
        "activity handler panicked; reporting retryable activity failure"
    );
    DispatchOutcome::Failed {
        failure: ActivityError::from(ActivityFailure::retryable(format!(
            "activity handler panicked: {message}"
        ))),
    }
}

fn panic_message(panic: &Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    String::from("unknown panic payload")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use aion_core::{
        ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload, WorkflowId,
    };
    use aion_proto::{ProtoActivityId, ProtoActivityTask, ProtoPayload, ProtoWorkflowId};
    use async_trait::async_trait;
    use futures::stream;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::{TypedActivityDispatcher, decode_payload, encode_payload};
    use crate::activity::ActivityFailure;
    use crate::config::WorkerConfig;
    use crate::error::WorkerError;
    use crate::protocol::{WorkerSession, WorkerTaskStream, validate_activity_handlers};
    use crate::runtime::serve_activity_tasks;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestInput {
        value: i32,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestOutput {
        doubled: i32,
    }

    #[derive(Default)]
    struct FakeSession {
        tasks: Vec<Result<ProtoActivityTask, WorkerError>>,
        reports: Vec<RecordedReport>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum RecordedReport {
        Completed(ActivityId, Payload),
        Failed(ActivityId, ActivityError),
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
            Box::pin(stream::iter(std::mem::take(&mut self.tasks)))
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
    fn payload_round_trip_preserves_typed_values_and_json_tag()
    -> Result<(), Box<dyn std::error::Error>> {
        let input = TestInput { value: 21 };
        let payload = encode_payload(&input)?;
        let decoded: TestInput = decode_payload(&payload)?;

        assert_eq!(payload.content_type(), &ContentType::Json);
        assert_eq!(decoded, input);

        let output = TestOutput { doubled: 42 };
        let output_payload = encode_payload(&output)?;
        let decoded_output: TestOutput = decode_payload(&output_payload)?;

        assert_eq!(output_payload.content_type(), &ContentType::Json);
        assert_eq!(decoded_output, output);
        Ok(())
    }

    #[tokio::test]
    async fn ok_handler_reports_completion_with_encoded_payload() -> Result<(), WorkerError> {
        let activity_id = ActivityId::from_sequence_position(1);
        let mut session = session_with_task(activity_id.clone(), "double", json!({"value": 21}))?;
        let dispatcher =
            TypedActivityDispatcher::new().register("double", |input: TestInput, context| {
                Box::pin(async move {
                    assert_eq!(context.attempt(), 1);
                    Ok(TestOutput {
                        doubled: input.value * 2,
                    })
                })
            });
        let config = test_config();

        serve_activity_tasks(&config, &mut session, Arc::new(dispatcher)).await?;

        let expected = encode_payload(&TestOutput { doubled: 42 })?;
        assert_eq!(
            session.reports,
            vec![RecordedReport::Completed(activity_id, expected)]
        );
        Ok(())
    }

    #[tokio::test]
    async fn retryable_handler_error_reports_retryable_failure() -> Result<(), WorkerError> {
        let activity_id = ActivityId::from_sequence_position(2);
        let mut session = session_with_task(activity_id.clone(), "flaky", json!({"value": 1}))?;
        let dispatcher =
            TypedActivityDispatcher::new().register("flaky", |input: TestInput, context| {
                Box::pin(async move {
                    drop((input, context));
                    Err::<TestOutput, ActivityFailure>(ActivityFailure::retryable("try again"))
                })
            });
        let config = test_config();

        serve_activity_tasks(&config, &mut session, Arc::new(dispatcher)).await?;

        assert_eq!(session.reports.len(), 1);
        assert_failure(
            &session.reports[0],
            &activity_id,
            ActivityErrorKind::Retryable,
            "try again",
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_handler_error_reports_terminal_failure() -> Result<(), WorkerError> {
        let activity_id = ActivityId::from_sequence_position(3);
        let mut session = session_with_task(activity_id.clone(), "invalid", json!({"value": 1}))?;
        let dispatcher =
            TypedActivityDispatcher::new().register("invalid", |input: TestInput, context| {
                Box::pin(async move {
                    drop((input, context));
                    Err::<TestOutput, ActivityFailure>(ActivityFailure::terminal("bad request"))
                })
            });
        let config = test_config();

        serve_activity_tasks(&config, &mut session, Arc::new(dispatcher)).await?;

        assert_eq!(session.reports.len(), 1);
        assert_failure(
            &session.reports[0],
            &activity_id,
            ActivityErrorKind::Terminal,
            "bad request",
        );
        Ok(())
    }

    #[tokio::test]
    async fn decode_failure_reports_terminal_failure() -> Result<(), WorkerError> {
        let activity_id = ActivityId::from_sequence_position(4);
        let mut session =
            session_with_task(activity_id.clone(), "double", json!({"value": "wrong"}))?;
        let dispatcher =
            TypedActivityDispatcher::new().register("double", |input: TestInput, context| {
                Box::pin(async move {
                    let _ = context;
                    Ok(TestOutput {
                        doubled: input.value * 2,
                    })
                })
            });
        let config = test_config();

        serve_activity_tasks(&config, &mut session, Arc::new(dispatcher)).await?;

        assert_eq!(session.reports.len(), 1);
        assert_failure(
            &session.reports[0],
            &activity_id,
            ActivityErrorKind::Terminal,
            "failed to decode activity input",
        );
        Ok(())
    }

    #[tokio::test]
    async fn panicking_handler_reports_retryable_failure_and_loop_survives()
    -> Result<(), WorkerError> {
        let activity_id = ActivityId::from_sequence_position(5);
        let mut session = session_with_task(activity_id.clone(), "panic", json!({"value": 1}))?;
        let dispatcher = TypedActivityDispatcher::new().register(
            "panic",
            |input: TestInput, context| -> super::HandlerFuture<TestOutput> {
                drop((input, context));
                Box::pin(futures::future::poll_fn(|context| {
                    let _ = context;
                    if std::env::var_os("AION_WORKER_TEST_DO_NOT_PANIC").is_some() {
                        return std::task::Poll::Ready(Ok(TestOutput { doubled: 0 }));
                    }
                    std::panic::panic_any(String::from("boom"));
                }))
            },
        );
        let config = test_config();

        serve_activity_tasks(&config, &mut session, Arc::new(dispatcher)).await?;

        assert_eq!(session.reports.len(), 1);
        assert_failure(
            &session.reports[0],
            &activity_id,
            ActivityErrorKind::Retryable,
            "activity handler panicked: boom",
        );
        Ok(())
    }

    #[tokio::test]
    async fn synchronous_handler_panic_reports_retryable_failure_and_loop_survives()
    -> Result<(), WorkerError> {
        let activity_id = ActivityId::from_sequence_position(6);
        let mut session =
            session_with_task(activity_id.clone(), "sync-panic", json!({"value": 1}))?;
        let dispatcher = TypedActivityDispatcher::new().register(
            "sync-panic",
            |input: TestInput, context| -> super::HandlerFuture<TestOutput> {
                drop((input, context));
                std::panic::panic_any("sync boom");
            },
        );
        let config = test_config();

        serve_activity_tasks(&config, &mut session, Arc::new(dispatcher)).await?;

        assert_eq!(session.reports.len(), 1);
        assert_failure(
            &session.reports[0],
            &activity_id,
            ActivityErrorKind::Retryable,
            "activity handler panicked: sync boom",
        );
        Ok(())
    }

    fn assert_failure(
        report: &RecordedReport,
        activity_id: &ActivityId,
        kind: ActivityErrorKind,
        message_contains: &str,
    ) {
        match report {
            RecordedReport::Failed(reported_id, failure) => {
                assert_eq!(reported_id, activity_id);
                assert_eq!(failure.kind, kind);
                assert!(failure.message.contains(message_contains));
            }
            RecordedReport::Completed(_, _) => panic!("expected failure report"),
        }
    }

    fn session_with_task(
        activity_id: ActivityId,
        activity_type: &str,
        input: serde_json::Value,
    ) -> Result<FakeSession, WorkerError> {
        let payload = Payload::from_json(&input).map_err(WorkerError::encode)?;
        Ok(FakeSession {
            tasks: vec![Ok(ProtoActivityTask {
                workflow_id: Some(ProtoWorkflowId::from(WorkflowId::new_v4())),
                activity_id: Some(ProtoActivityId::from(activity_id)),
                activity_type: activity_type.to_owned(),
                input: Some(ProtoPayload::from(payload)),
            })],
            reports: Vec::new(),
        })
    }

    fn test_config() -> WorkerConfig {
        WorkerConfig::new("http://127.0.0.1:50051", "payments", "worker-a", 1, None)
    }
}
