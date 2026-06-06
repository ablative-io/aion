//! `Activity` trait, `ActivityFailure`, and typed registration.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;

use aion_core::{ActivityError, ActivityErrorKind, Payload};
use async_trait::async_trait;
use futures::FutureExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::error;

use crate::context::ActivityContext;
use crate::error::{MissingActivityHandler, WorkerError};
use crate::protocol::ActivityTask;
use crate::runtime::loop_::{ActivityDispatcher, DispatchOutcome};

/// Explicit retryability classification for an activity failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classification {
    /// The engine may retry the activity according to policy.
    Retryable,
    /// The activity failure is permanent and must not be retried.
    Terminal,
}

/// Handler-returned failure with explicit retryability classification.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ActivityFailure {
    classification: Classification,
    message: String,
    detail: Option<Payload>,
}

impl ActivityFailure {
    /// Creates a retryable activity failure.
    #[must_use]
    pub fn retryable(message: impl Into<String>) -> Self {
        Self::new(Classification::Retryable, message, None)
    }

    /// Creates a terminal activity failure.
    #[must_use]
    pub fn terminal(message: impl Into<String>) -> Self {
        Self::new(Classification::Terminal, message, None)
    }

    /// Attaches opaque structured detail to this failure.
    #[must_use]
    pub fn with_detail(mut self, detail: Payload) -> Self {
        self.detail = Some(detail);
        self
    }

    /// Returns the explicit retryability classification.
    #[must_use]
    pub const fn classification(&self) -> &Classification {
        &self.classification
    }

    /// Returns the human-readable failure message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the optional structured failure detail.
    #[must_use]
    pub const fn detail(&self) -> Option<&Payload> {
        self.detail.as_ref()
    }

    fn new(
        classification: Classification,
        message: impl Into<String>,
        detail: Option<Payload>,
    ) -> Self {
        Self {
            classification,
            message: message.into(),
            detail,
        }
    }
}

impl From<Classification> for ActivityErrorKind {
    fn from(value: Classification) -> Self {
        match value {
            Classification::Retryable => Self::Retryable,
            Classification::Terminal => Self::Terminal,
        }
    }
}

impl From<ActivityFailure> for ActivityError {
    fn from(value: ActivityFailure) -> Self {
        Self {
            kind: ActivityErrorKind::from(value.classification),
            message: value.message,
            details: value.detail,
        }
    }
}

/// Boxed future returned by a typed activity handler.
pub type HandlerFuture<'context, Output> =
    Pin<Box<dyn Future<Output = Result<Output, ActivityFailure>> + Send + 'context>>;

type BoxedHandler<Input, Output> = Box<
    dyn for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
        + Send
        + Sync,
>;

/// Registry of typed activity handlers keyed by activity-type name.
#[derive(Default)]
pub struct ActivityRegistry {
    handlers: BTreeMap<String, Box<dyn ErasedActivityHandler>>,
}

impl ActivityRegistry {
    /// Creates an empty activity registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one typed activity handler under an activity-type name.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError::Registration`] when the name is already registered.
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
        let activity_type = activity_type.into();
        if self.handlers.contains_key(&activity_type) {
            return Err(WorkerError::registration(DuplicateActivityType {
                activity_type,
            }));
        }
        self.handlers
            .insert(activity_type, Box::new(TypedHandler::new(handler)));
        Ok(self)
    }

    /// Returns true when no activity handlers have been registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Returns the registered activity-type names in deterministic order.
    #[must_use]
    pub fn activity_types(&self) -> BTreeSet<String> {
        self.handlers.keys().cloned().collect()
    }
}

#[async_trait]
impl ActivityDispatcher for ActivityRegistry {
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError> {
        let Some(handler) = self.handlers.get(&task.activity_type) else {
            return Err(WorkerError::registration(MissingActivityHandler {
                activity_type: task.activity_type,
            }));
        };
        handler.dispatch(task, context).await
    }

    fn activity_types(&self) -> BTreeSet<String> {
        self.activity_types()
    }
}

/// Backwards-compatible name for the typed activity registry used by the runtime.
pub type TypedActivityDispatcher = ActivityRegistry;

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
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError>;
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
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError> {
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
        let handler_future =
            match std::panic::catch_unwind(AssertUnwindSafe(|| (self.handler)(input, &context))) {
                Ok(handler_future) => handler_future,
                Err(panic) => return Ok(panic_failure(&task, &panic)),
            };
        let handler_result = AssertUnwindSafe(handler_future).catch_unwind().await;
        let outcome = match handler_result {
            Ok(Ok(output)) => DispatchOutcome::Completed {
                output: encode_payload(&output)?,
            },
            Ok(Err(failure)) => DispatchOutcome::Failed {
                failure: ActivityError::from(failure),
            },
            Err(panic) => panic_failure(&task, &panic),
        };
        Ok(outcome)
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

/// Error returned when an activity type is registered more than once.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("activity type `{activity_type}` already has a registered handler")]
pub struct DuplicateActivityType {
    /// Duplicate activity type name.
    pub activity_type: String,
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityError, ActivityId, ContentType, WorkflowId};
    use aion_proto::{
        ProtoActivityError, ProtoActivityErrorKind, ProtoActivityId, ProtoActivityTask,
        ProtoPayload, ProtoWorkflowId,
    };
    use serde::{Deserialize, Serialize};

    use super::{ActivityFailure, ActivityRegistry, decode_payload, encode_payload};
    use crate::WorkerError;
    use crate::runtime::{ActivityDispatcher, DispatchOutcome};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestInput {
        value: i32,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestOutput {
        doubled: i32,
    }

    #[test]
    fn retryable_and_terminal_failures_map_to_distinct_wire_classifications() {
        let retryable = ActivityFailure::retryable("temporary outage");
        let terminal = ActivityFailure::terminal("invalid request");

        let retryable_core = ActivityError::from(retryable);
        let terminal_core = ActivityError::from(terminal);
        let retryable_wire = ProtoActivityError::from(retryable_core);
        let terminal_wire = ProtoActivityError::from(terminal_core);

        assert_eq!(
            retryable_wire.kind,
            ProtoActivityErrorKind::Retryable as i32
        );
        assert_eq!(terminal_wire.kind, ProtoActivityErrorKind::Terminal as i32);
    }

    #[tokio::test]
    async fn typed_activity_round_trips_through_registry() -> Result<(), WorkerError> {
        let registry =
            ActivityRegistry::new().register_activity("double", |input: TestInput, context| {
                Box::pin(async move {
                    assert_eq!(context.attempt(), 1);
                    Ok(TestOutput {
                        doubled: input.value * 2,
                    })
                })
            })?;
        let task = proto_task("double", &TestInput { value: 21 })?;
        let (context, cancellation) = crate::ActivityContext::for_workflow(
            Some(WorkflowId::new_v4()),
            ActivityId::from_sequence_position(99),
            1,
            None,
        );
        drop(cancellation);

        let outcome = registry.dispatch(task.try_into()?, context).await?;

        let DispatchOutcome::Completed { output } = outcome else {
            return Err(WorkerError::decode(UnexpectedFailure));
        };
        assert_eq!(output.content_type(), &ContentType::Json);
        let decoded: TestOutput = decode_payload(&output)?;
        assert_eq!(decoded, TestOutput { doubled: 42 });
        Ok(())
    }

    #[test]
    fn duplicate_activity_registration_is_rejected() -> Result<(), WorkerError> {
        let registry =
            ActivityRegistry::new().register_activity("double", |input: TestInput, context| {
                Box::pin(async move {
                    let _ = context;
                    Ok(TestOutput {
                        doubled: input.value * 2,
                    })
                })
            })?;

        let error = registry
            .register_activity("double", |input: TestInput, context| {
                Box::pin(async move {
                    let _ = context;
                    Ok(TestOutput {
                        doubled: input.value,
                    })
                })
            })
            .err()
            .ok_or_else(|| WorkerError::decode(UnexpectedFailure))?;

        assert!(
            error
                .to_string()
                .contains("already has a registered handler")
        );
        Ok(())
    }

    fn proto_task(
        activity_type: &str,
        input: &TestInput,
    ) -> Result<ProtoActivityTask, WorkerError> {
        Ok(ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(WorkflowId::new_v4())),
            activity_id: Some(ProtoActivityId::from(ActivityId::from_sequence_position(1))),
            activity_type: activity_type.to_owned(),
            input: Some(ProtoPayload::from(encode_payload(&input)?)),
        })
    }

    #[derive(Debug, thiserror::Error)]
    #[error("expected completed activity outcome")]
    struct UnexpectedFailure;
}
