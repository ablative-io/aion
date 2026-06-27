//! Transport-agnostic dev-server handlers over the REAL engine, store, and
//! event stream.
//!
//! Every operation here drives the same production engine surface the public
//! transports drive — there is no dev-only engine and no execution path whose
//! semantics diverge from production (CN4):
//!
//! * [`trigger_run`] starts a workflow through the same
//!   [`crate::api::handlers::workflows::start`] path `/workflows/start` uses,
//!   and returns the firehose subscription a client uses to watch the run live
//!   over the existing `/events/stream` WebSocket (no second stream is built);
//! * [`register_mock`] installs an opt-in per-run activity mock in the shared
//!   [`ActivityMockRegistry`] the engine's dispatcher already consults — the
//!   engine is untouched;
//! * [`replay_run`] re-drives a failed run by reading its recorded start
//!   (workflow type and input) from the real store and starting a fresh run of
//!   the same workflow through the real engine — no mock-only re-execution.

use aion_core::{Event, WorkflowId, WorkflowStatus, WorkflowSummary};
use aion_proto::{ProtoDescribeWorkflowRequest, ProtoStartWorkflowRequest, WireError};
use serde::{Deserialize, Serialize};

use crate::api::handlers::start;
use crate::{CallerIdentity, NamespaceOperation, ServerState, WorkflowTarget};

use super::mock::MockedActivity;

/// Builds the anti-existence-leak not-found wire error for a workflow.
fn workflow_not_found(workflow_id: &WorkflowId) -> WireError {
    WireError::not_found(format!("workflow {workflow_id} not found"))
        .with_error_type("WorkflowNotFound")
}

/// Scopes a dev operation to a target workflow: authorizes the caller for the
/// namespace, verifies the workflow is visible in it (foreign and nonexistent
/// are indistinguishable), and yields the engine to read or drive it.
async fn scope_to_workflow(
    state: &ServerState,
    caller: &CallerIdentity,
    namespace: &str,
    workflow_id: &WorkflowId,
) -> Result<crate::namespace::ScopedEngine, WireError> {
    let describe = ProtoDescribeWorkflowRequest {
        namespace: namespace.to_owned(),
        workflow_id: Some(aion_proto::ProtoWorkflowId::from(workflow_id.clone())),
        run_id: None,
        include_history: false,
    };
    let operation = NamespaceOperation::describe(&describe, WorkflowTarget::workflow(workflow_id));
    state
        .namespace_guard()
        .scope(caller, &operation)
        .await
        .map_err(|error| error.to_wire_error())
}

/// Request to trigger a fresh workflow run from the dev server.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerRunRequest {
    /// Namespace the run is scoped to.
    pub namespace: String,
    /// Registered workflow type to start.
    pub workflow_type: String,
    /// JSON input payload for the workflow.
    pub input: serde_json::Value,
}

/// The started run plus the firehose subscription that streams its events.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerRunResponse {
    /// Started workflow id.
    pub workflow_id: String,
    /// Started run id.
    pub run_id: String,
    /// The exact `subscribe` frame a client sends on the existing
    /// `/events/stream` WebSocket to watch this run live — the dev server reuses
    /// the production firehose rather than opening a second stream.
    pub stream_subscription: StreamSubscription,
}

/// The per-workflow subscription frame for the existing event-stream socket.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamSubscription {
    /// WebSocket path the client connects to (the existing firehose).
    pub path: String,
    /// The subscription request body to send once connected.
    pub subscribe: serde_json::Value,
}

/// Triggers a run and returns the existing-firehose subscription for it.
///
/// # Errors
///
/// Returns a [`WireError`] when the input payload is malformed or the real
/// start path rejects the request (unknown type, namespace denial, engine
/// failure).
pub async fn trigger_run(
    state: &ServerState,
    caller: &CallerIdentity,
    request: TriggerRunRequest,
) -> Result<TriggerRunResponse, WireError> {
    let input = aion_core::Payload::from_json(&request.input)
        .map_err(|error| WireError::invalid_input(format!("invalid run input JSON: {error}")))?;
    let start_request = ProtoStartWorkflowRequest {
        namespace: request.namespace.clone(),
        workflow_type: request.workflow_type.clone(),
        input: Some(input.into()),
        routing_key: None,
    };
    let response = start(state.namespace_guard(), caller, start_request).await?;
    let workflow_id = response
        .workflow_id
        .ok_or_else(|| WireError::backend("start response missing workflow id"))?;
    let run_id = response
        .run_id
        .ok_or_else(|| WireError::backend("start response missing run id"))?;
    let workflow_id = WorkflowId::try_from(workflow_id)?;
    let run_id = aion_core::RunId::try_from(run_id)?;

    Ok(TriggerRunResponse {
        stream_subscription: per_workflow_subscription(&request.namespace, &workflow_id),
        workflow_id: workflow_id.to_string(),
        run_id: run_id.to_string(),
    })
}

/// Builds the `subscribe` frame for the existing `/events/stream` firehose,
/// scoped to one workflow.
fn per_workflow_subscription(namespace: &str, workflow_id: &WorkflowId) -> StreamSubscription {
    StreamSubscription {
        path: "/events/stream".to_owned(),
        subscribe: serde_json::json!({
            "type": "subscribe",
            "subscription": {
                "per_workflow": {
                    "namespace": namespace,
                    "workflow_id": workflow_id.to_string(),
                }
            }
        }),
    }
}

/// Outcome a per-run activity mock should produce.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MockOutcome {
    /// The activity succeeds, returning this JSON-encoded typed result.
    Succeeds {
        /// JSON value returned verbatim to the workflow.
        result: serde_json::Value,
    },
    /// The activity fails with this message.
    Fails {
        /// Failure message returned to the workflow.
        message: String,
    },
}

/// Request to install an opt-in per-run activity mock.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterMockRequest {
    /// Namespace the target run is scoped to (authorized like any operation).
    pub namespace: String,
    /// The run whose activity is mocked.
    pub workflow_id: String,
    /// The named activity to mock.
    pub activity_name: String,
    /// The canned outcome the mocked activity returns.
    pub outcome: MockOutcome,
}

/// Acknowledgement of an installed mock.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisterMockResponse {
    /// The run the mock was installed for.
    pub workflow_id: String,
    /// The mocked activity name.
    pub activity_name: String,
}

/// Installs an opt-in per-run activity mock for one run, without changing the
/// engine.
///
/// # Errors
///
/// Returns a [`WireError`] when the workflow id is malformed, the caller is not
/// authorized for the namespace, the JSON result cannot be encoded, the dev
/// surface holds no mock registry (a wiring fault), or the registry mutex is
/// poisoned.
pub async fn register_mock(
    state: &ServerState,
    caller: &CallerIdentity,
    request: RegisterMockRequest,
) -> Result<RegisterMockResponse, WireError> {
    let workflow_id = WorkflowId::try_from(parse_workflow_id(&request.workflow_id)?)?;
    // Authorize the caller for the run's namespace and confirm the run is
    // visible in it, using the same guard every run operation uses; a foreign
    // namespace is denied and a foreign workflow is not-found, identically.
    scope_to_workflow(state, caller, &request.namespace, &workflow_id).await?;

    let registry = state
        .activity_mock_registry()
        .ok_or_else(|| WireError::backend("dev activity mocking is not enabled on this server"))?;
    let mock = match request.outcome {
        MockOutcome::Succeeds { result } => {
            let payload = aion_core::Payload::from_json(&result).map_err(|error| {
                WireError::invalid_input(format!("invalid mock result JSON: {error}"))
            })?;
            let result_json = String::from_utf8(payload.bytes().to_vec()).map_err(|error| {
                WireError::invalid_input(format!("mock result is not valid UTF-8 JSON: {error}"))
            })?;
            MockedActivity::Succeeds { result_json }
        }
        MockOutcome::Fails { message } => MockedActivity::Fails { message },
    };
    registry
        .register(workflow_id.clone(), request.activity_name.clone(), mock)
        .map_err(WireError::backend)?;

    tracing::info!(
        operation = "dev.register_mock",
        subject = caller.subject(),
        namespace = %request.namespace,
        workflow_id = %workflow_id,
        activity_name = %request.activity_name,
        "dev activity mock registered"
    );
    Ok(RegisterMockResponse {
        workflow_id: workflow_id.to_string(),
        activity_name: request.activity_name,
    })
}

/// Request to replay a failed run through the real engine.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRunRequest {
    /// Namespace the failed run is scoped to.
    pub namespace: String,
    /// The failed run to replay.
    pub workflow_id: String,
}

/// The fresh run started to replay a failed one.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayRunResponse {
    /// The original failed run that was replayed.
    pub replayed_workflow_id: String,
    /// The workflow type re-driven from the recorded start.
    pub workflow_type: String,
    /// The fresh run started through the real engine.
    pub workflow_id: String,
    /// The fresh run id.
    pub run_id: String,
    /// The firehose subscription for the fresh run.
    pub stream_subscription: StreamSubscription,
}

/// Replays a failed run by re-driving it through the real engine and store.
///
/// Reads the failed run's recorded `WorkflowStarted` (workflow type and input)
/// from the real store and starts a fresh run of the same workflow through the
/// real engine. There is no separate mock-only engine and no divergent
/// execution path (CN4): the replay is an ordinary start of the same code with
/// the same input.
///
/// # Errors
///
/// Returns a [`WireError`] when the workflow id is malformed, the caller is not
/// authorized, the run is unknown, the run is not in a failed terminal state,
/// its recorded start cannot be read, or the real start path fails.
pub async fn replay_run(
    state: &ServerState,
    caller: &CallerIdentity,
    request: ReplayRunRequest,
) -> Result<ReplayRunResponse, WireError> {
    let workflow_id = WorkflowId::try_from(parse_workflow_id(&request.workflow_id)?)?;
    let scoped = scope_to_workflow(state, caller, &request.namespace, &workflow_id).await?;
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;

    let history = engine
        .store()
        .read_history(&workflow_id)
        .await
        .map_err(|error| crate::ServerError::from(error).to_wire_error())?;
    let summary =
        WorkflowSummary::from_history(&history).ok_or_else(|| workflow_not_found(&workflow_id))?;
    if summary.status != WorkflowStatus::Failed {
        return Err(WireError::invalid_input(format!(
            "workflow {workflow_id} is {:?}, not Failed; only a failed run can be replayed",
            summary.status
        )));
    }
    let (workflow_type, input) = recorded_start(&history).ok_or_else(|| {
        WireError::backend(format!(
            "workflow {workflow_id} has no recorded start event to replay from"
        ))
    })?;

    let start_request = ProtoStartWorkflowRequest {
        namespace: request.namespace.clone(),
        workflow_type: workflow_type.clone(),
        input: Some(input.into()),
        routing_key: None,
    };
    let response = start(state.namespace_guard(), caller, start_request).await?;
    let fresh_workflow_id = WorkflowId::try_from(
        response
            .workflow_id
            .ok_or_else(|| WireError::backend("replay start response missing workflow id"))?,
    )?;
    let fresh_run_id = aion_core::RunId::try_from(
        response
            .run_id
            .ok_or_else(|| WireError::backend("replay start response missing run id"))?,
    )?;

    tracing::info!(
        operation = "dev.replay_run",
        subject = caller.subject(),
        namespace = %request.namespace,
        replayed_workflow_id = %workflow_id,
        workflow_id = %fresh_workflow_id,
        workflow_type = %workflow_type,
        "dev replay re-drove a failed run through the real engine"
    );
    Ok(ReplayRunResponse {
        replayed_workflow_id: workflow_id.to_string(),
        stream_subscription: per_workflow_subscription(&request.namespace, &fresh_workflow_id),
        workflow_id: fresh_workflow_id.to_string(),
        run_id: fresh_run_id.to_string(),
        workflow_type,
    })
}

/// Extracts the recorded workflow type and input from a run's first
/// `WorkflowStarted` event.
fn recorded_start(history: &[Event]) -> Option<(String, aion_core::Payload)> {
    history.iter().find_map(|event| match event {
        Event::WorkflowStarted {
            workflow_type,
            input,
            ..
        } => Some((workflow_type.clone(), input.clone())),
        _ => None,
    })
}

/// Parses a workflow id string into the proto id, surfacing a malformed id as
/// an invalid-input wire error rather than a panic.
fn parse_workflow_id(raw: &str) -> Result<aion_proto::ProtoWorkflowId, WireError> {
    let uuid = uuid::Uuid::parse_str(raw).map_err(|error| {
        WireError::invalid_input(format!("invalid workflow id `{raw}`: {error}"))
    })?;
    Ok(aion_proto::ProtoWorkflowId::from(WorkflowId::new(uuid)))
}

#[cfg(test)]
mod tests {
    use aion_core::{Event, Payload};

    use super::{
        MockOutcome, RegisterMockRequest, TriggerRunRequest, parse_workflow_id, recorded_start,
    };

    #[test]
    fn trigger_request_rejects_unknown_fields() {
        let raw = r#"{"namespace":"default","workflow_type":"order","input":{},"extra":1}"#;
        assert!(serde_json::from_str::<TriggerRunRequest>(raw).is_err());
    }

    #[test]
    fn mock_outcome_parses_succeeds_and_fails() -> Result<(), serde_json::Error> {
        let succeeds: RegisterMockRequest = serde_json::from_str(
            r#"{"namespace":"default","workflow_id":"00000000-0000-0000-0000-000000000001","activity_name":"charge","outcome":{"kind":"succeeds","result":{"ok":true}}}"#,
        )?;
        assert!(matches!(succeeds.outcome, MockOutcome::Succeeds { .. }));
        let fails: RegisterMockRequest = serde_json::from_str(
            r#"{"namespace":"default","workflow_id":"00000000-0000-0000-0000-000000000001","activity_name":"charge","outcome":{"kind":"fails","message":"declined"}}"#,
        )?;
        assert!(matches!(fails.outcome, MockOutcome::Fails { .. }));
        Ok(())
    }

    #[test]
    fn parse_workflow_id_rejects_a_non_uuid() {
        assert!(parse_workflow_id("not-a-uuid").is_err());
        assert!(parse_workflow_id("00000000-0000-0000-0000-000000000001").is_ok());
    }

    #[test]
    fn recorded_start_extracts_type_and_input() -> Result<(), Box<dyn std::error::Error>> {
        let payload = Payload::from_json(&serde_json::json!({"amount": 1}))?;
        let started = Event::WorkflowStarted {
            envelope: aion_core::EventEnvelope {
                seq: 1,
                recorded_at: chrono::DateTime::from_timestamp(0, 0).unwrap_or_default(),
                workflow_id: aion_core::WorkflowId::new(uuid::Uuid::from_u128(1)),
            },
            workflow_type: "order".to_owned(),
            input: payload.clone(),
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(2)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        };
        let extracted = recorded_start(std::slice::from_ref(&started));
        assert_eq!(extracted, Some(("order".to_owned(), payload)));
        assert!(recorded_start(&[]).is_none());
        Ok(())
    }
}
