//! shared handler layer over Engine

use aion_core::{Payload, RunId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoDescribeWorkflowRequest,
    ProtoDescribeWorkflowResponse, ProtoListWorkflowsRequest, ProtoListWorkflowsResponse,
    ProtoQueryRequest, ProtoQueryResponse, ProtoSignalRequest, ProtoSignalResponse,
    ProtoStartWorkflowRequest, ProtoStartWorkflowResponse, WireError,
    convert::{ProtoPayload, decode_workflow_filter, encode_event, encode_workflow_summary},
    proto_query_response,
};

use crate::{CallerIdentity, NamespaceGuard, NamespaceOperation, ServerError, WorkflowTarget};

/// Handles a decoded start-workflow request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the payload is missing or malformed, namespace scoping fails,
/// the engine start call fails, or namespace ownership metadata cannot be recorded.
pub async fn start(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoStartWorkflowRequest,
) -> Result<ProtoStartWorkflowResponse, WireError> {
    let scoped = guard
        .scope(caller, &NamespaceOperation::start(&request))
        .map_err(|error| error.to_wire_error())?;
    let input = required_payload(request.input.clone())?;
    let handle = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .start_workflow(&request.workflow_type, input)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    scoped
        .record_workflow(handle.workflow_id().clone())
        .map_err(|error| error.to_wire_error())?;

    Ok(ProtoStartWorkflowResponse {
        workflow_id: Some(handle.workflow_id().clone().into()),
        run_id: Some(handle.run_id().clone().into()),
    })
}

/// Handles a decoded signal request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when IDs or payloads are missing or malformed, namespace scoping
/// fails, or the engine signal call fails.
pub async fn signal(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoSignalRequest,
) -> Result<ProtoSignalResponse, WireError> {
    let workflow_id = required_workflow_id(request.workflow_id.clone())?;
    let run_id = required_run_id(request.run_id.clone())?;
    let payload = required_payload(request.payload.clone())?;
    let target = WorkflowTarget::with_run(&workflow_id, &run_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::signal(&request, target))
        .map_err(|error| error.to_wire_error())?;

    scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .signal(&workflow_id, &run_id, request.signal_name, payload)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoSignalResponse {})
}

/// Handles a decoded query request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when IDs are missing or malformed, namespace scoping fails, or the
/// engine query call fails.
pub async fn query(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoQueryRequest,
) -> Result<ProtoQueryResponse, WireError> {
    let workflow_id = required_workflow_id(request.workflow_id.clone())?;
    let run_id = required_run_id(request.run_id.clone())?;
    let target = WorkflowTarget::with_run(&workflow_id, &run_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::query(&request, target))
        .map_err(|error| error.to_wire_error())?;

    let result = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .query(&workflow_id, &run_id, request.query_name)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoQueryResponse {
        outcome: Some(proto_query_response::Outcome::Result(result.into())),
    })
}

/// Handles a decoded cancel request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when IDs are missing or malformed, namespace scoping fails, or the
/// engine cancel call fails.
pub async fn cancel(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoCancelRequest,
) -> Result<ProtoCancelResponse, WireError> {
    let workflow_id = required_workflow_id(request.workflow_id.clone())?;
    let run_id = required_run_id(request.run_id.clone())?;
    let target = WorkflowTarget::with_run(&workflow_id, &run_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::cancel(&request, target))
        .map_err(|error| error.to_wire_error())?;

    scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .cancel(&workflow_id, &run_id, request.reason)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoCancelResponse {})
}

/// Handles a decoded list-workflows request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the filter envelope is malformed, namespace scoping fails, the
/// engine list call fails, or summaries cannot be encoded.
pub async fn list(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoListWorkflowsRequest,
) -> Result<ProtoListWorkflowsResponse, WireError> {
    let scope_filter = WorkflowFilter::default();
    let scoped = guard
        .scope(caller, &NamespaceOperation::list(&request, &scope_filter))
        .map_err(|error| error.to_wire_error())?;
    let filter = decode_filter(request.filter.as_ref())?;

    let summaries = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .list_workflows(filter)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    let namespace = scoped.namespace().to_owned();
    let summaries = summaries
        .into_iter()
        .map(|summary| encode_workflow_summary(namespace.clone(), None, &summary))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProtoListWorkflowsResponse { summaries })
}

/// Handles a decoded describe-workflow request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when IDs are missing or malformed, namespace scoping fails, store
/// history reading fails, the workflow has no summary, or response envelopes cannot be encoded.
pub async fn describe(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoDescribeWorkflowRequest,
) -> Result<ProtoDescribeWorkflowResponse, WireError> {
    let workflow_id = required_workflow_id(request.workflow_id.clone())?;
    let run_id = required_run_id(request.run_id.clone())?;
    let target = WorkflowTarget::with_run(&workflow_id, &run_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::describe(&request, target))
        .map_err(|error| error.to_wire_error())?;

    let history = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .store()
        .read_history(&workflow_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let summary = WorkflowSummary::from_history(&history)
        .ok_or_else(|| WireError::not_found("workflow not found"))?;
    let namespace = scoped.namespace().to_owned();
    let summary = encode_workflow_summary(namespace.clone(), None, &summary)?;
    let history = encode_history(request.include_history, &namespace, &history)?;

    Ok(ProtoDescribeWorkflowResponse {
        summary: Some(summary),
        history,
    })
}

fn required_workflow_id(id: Option<aion_proto::ProtoWorkflowId>) -> Result<WorkflowId, WireError> {
    id.ok_or_else(|| WireError::backend("workflow id is missing"))?
        .try_into()
}

fn required_run_id(id: Option<aion_proto::ProtoRunId>) -> Result<RunId, WireError> {
    id.ok_or_else(|| WireError::backend("run id is missing"))?
        .try_into()
}

fn required_payload(payload: Option<ProtoPayload>) -> Result<Payload, WireError> {
    payload
        .ok_or_else(|| WireError::backend("payload is missing"))?
        .try_into()
}

fn decode_filter(filter: Option<&aion_proto::WireEnvelope>) -> Result<WorkflowFilter, WireError> {
    filter.map_or_else(|| Ok(WorkflowFilter::default()), decode_workflow_filter)
}

fn encode_history(
    include_history: bool,
    namespace: &str,
    history: &[aion_core::Event],
) -> Result<Vec<aion_proto::WireEnvelope>, WireError> {
    if include_history {
        history
            .iter()
            .map(|event| encode_event(namespace.to_owned(), None, event))
            .collect()
    } else {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion::{Engine, EngineBuilder};
    use aion_core::{Event, EventEnvelope, Payload, WorkflowStatus};
    use aion_proto::{
        WireErrorCode,
        convert::{decode_event, decode_workflow_summary, encode_workflow_filter},
    };
    use aion_store::{EventStore, InMemoryStore};
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::{NamespaceResolver, WorkflowOwnership, config::NamespaceMode};

    const NAMESPACE: &str = "tenant-a";

    #[tokio::test]
    async fn start_handler_scopes_then_invokes_engine_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let request = ProtoStartWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_type: "missing-workflow".to_owned(),
            input: Some(proto_payload()?),
        };

        let error = start(&context.guard, &context.caller, request).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::Backend)
        );
        Ok(())
    }

    #[tokio::test]
    async fn signal_handler_scopes_then_invokes_engine_signal()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = signal(&context.guard, &context.caller, signal_request()?).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NotFound)
        );
        Ok(())
    }

    #[tokio::test]
    async fn query_handler_scopes_then_invokes_engine_query()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = query(&context.guard, &context.caller, query_request()).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NotFound)
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancel_handler_scopes_then_invokes_engine_cancel()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = cancel(&context.guard, &context.caller, cancel_request()).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NotFound)
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_handler_scopes_then_invokes_engine_list() -> Result<(), Box<dyn std::error::Error>>
    {
        let context = context().await?;
        append_started(context.store.as_ref()).await?;
        let request = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(encode_workflow_filter(
                NAMESPACE,
                None,
                &WorkflowFilter {
                    status: Some(WorkflowStatus::Running),
                    ..WorkflowFilter::default()
                },
            )?),
        };

        let response = list(&context.guard, &context.caller, request).await?;

        assert_eq!(response.summaries.len(), 1);
        let summary = decode_workflow_summary(&response.summaries[0])?;
        assert_eq!(summary.workflow_id, workflow_id());
        Ok(())
    }

    #[tokio::test]
    async fn describe_handler_scopes_then_reads_summary_and_optional_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_started(context.store.as_ref()).await?;

        let response = describe(&context.guard, &context.caller, describe_request(true)).await?;

        let summary = response
            .summary
            .as_ref()
            .map(decode_workflow_summary)
            .transpose()?
            .ok_or_else(|| WireError::backend("summary missing"))?;
        assert_eq!(summary.workflow_id, workflow_id());
        assert_eq!(response.history.len(), 1);
        assert!(matches!(
            decode_event(&response.history[0])?,
            Event::WorkflowStarted { .. }
        ));
        Ok(())
    }

    #[tokio::test]
    async fn describe_handler_maps_empty_history_to_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = describe(&context.guard, &context.caller, describe_request(false)).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NotFound)
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_handler_returns_namespace_denied_before_engine_access()
    -> Result<(), Box<dyn std::error::Error>> {
        let ownership = WorkflowOwnership::default();
        let resolver =
            NamespaceResolver::authorization_only(NamespaceMode::SharedEngine, ownership);
        let guard = NamespaceGuard::new(resolver);
        let caller = CallerIdentity::new("alice", [String::from("tenant-b")]);
        let request = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: None,
        };

        let error = list(&guard, &caller, request).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NamespaceDenied)
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_start_does_not_decode_missing_payload_before_namespace_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let (guard, caller) = denied_guard();
        let request = ProtoStartWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_type: "fixture".to_owned(),
            input: None,
        };

        let error = start(&guard, &caller, request).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NamespaceDenied)
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_list_does_not_decode_malformed_filter_before_namespace_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let (guard, caller) = denied_guard();
        let request = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(aion_proto::WireEnvelope {
                namespace: NAMESPACE.to_owned(),
                request_id: None,
                payload: Some(ProtoPayload {
                    content_type: "application/octet-stream".to_owned(),
                    bytes: Vec::new(),
                }),
            }),
        };

        let error = list(&guard, &caller, request).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NamespaceDenied)
        );
        Ok(())
    }

    struct TestContext {
        guard: NamespaceGuard,
        caller: CallerIdentity,
        ownership: WorkflowOwnership,
        store: Arc<dyn EventStore>,
    }

    async fn context() -> Result<TestContext, aion::EngineError> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(Arc::clone(&store))
                .scheduler_threads(1)
                .build()
                .await?,
        );
        Ok(context_from_engine(engine, store))
    }

    fn context_from_engine(engine: Arc<Engine>, store: Arc<dyn EventStore>) -> TestContext {
        let ownership = WorkflowOwnership::default();
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            ownership.clone(),
        );
        TestContext {
            guard: NamespaceGuard::new(resolver),
            caller: CallerIdentity::new("alice", [NAMESPACE.to_owned()]),
            ownership,
            store,
        }
    }

    fn denied_guard() -> (NamespaceGuard, CallerIdentity) {
        let ownership = WorkflowOwnership::default();
        let resolver =
            NamespaceResolver::authorization_only(NamespaceMode::SharedEngine, ownership);
        let guard = NamespaceGuard::new(resolver);
        let caller = CallerIdentity::new("alice", [String::from("tenant-b")]);
        (guard, caller)
    }

    async fn append_started(store: &dyn EventStore) -> Result<(), Box<dyn std::error::Error>> {
        let event = started_event()?;
        store.append(&workflow_id(), &[event], 0).await?;
        Ok(())
    }

    fn signal_request() -> Result<ProtoSignalRequest, aion_core::PayloadError> {
        Ok(ProtoSignalRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(workflow_id().into()),
            run_id: Some(run_id().into()),
            signal_name: "poke".to_owned(),
            payload: Some(proto_payload()?),
        })
    }

    fn query_request() -> ProtoQueryRequest {
        ProtoQueryRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(workflow_id().into()),
            run_id: Some(run_id().into()),
            query_name: "state".to_owned(),
        }
    }

    fn cancel_request() -> ProtoCancelRequest {
        ProtoCancelRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(workflow_id().into()),
            run_id: Some(run_id().into()),
            reason: "test cancellation".to_owned(),
        }
    }

    fn describe_request(include_history: bool) -> ProtoDescribeWorkflowRequest {
        ProtoDescribeWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(workflow_id().into()),
            run_id: Some(run_id().into()),
            include_history,
        }
    }

    fn started_event() -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id(),
            },
            workflow_type: "fixture".to_owned(),
            input: payload()?,
        })
    }

    fn proto_payload() -> Result<ProtoPayload, aion_core::PayloadError> {
        Ok(payload()?.into())
    }

    fn payload() -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "fixture": "input" }))
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(uuid::Uuid::from_u128(1))
    }

    fn run_id() -> RunId {
        RunId::new(uuid::Uuid::from_u128(2))
    }
}
