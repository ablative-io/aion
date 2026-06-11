//! shared handler layer over Engine

use aion_core::{Payload, RunId, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary};
use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoCountWorkflowsRequest,
    ProtoCountWorkflowsResponse, ProtoDescribeWorkflowRequest, ProtoDescribeWorkflowResponse,
    ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoQueryRequest, ProtoQueryResponse,
    ProtoSignalRequest, ProtoSignalResponse, ProtoStartWorkflowRequest, ProtoStartWorkflowResponse,
    WireError,
    convert::{
        ProtoPayload, decode_core_value, encode_core_value, encode_event, encode_workflow_summary,
    },
    proto_query_response,
};

use crate::{CallerIdentity, NamespaceGuard, NamespaceOperation, ServerError, WorkflowTarget};
use tracing::{Instrument, info_span};

/// Handles a decoded start-workflow request.
///
/// The authorized namespace is recorded durably as the `aion.namespace` search
/// attribute in the same atomic append as the workflow's start event, so
/// ownership survives server restarts and is never tracked only in memory.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the payload is missing or malformed, namespace scoping fails,
/// or the engine start call fails.
pub async fn start(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoStartWorkflowRequest,
) -> Result<ProtoStartWorkflowResponse, WireError> {
    let scoped = guard
        .scope(caller, &NamespaceOperation::start(&request))
        .await
        .map_err(|error| error.to_wire_error())?;
    let namespace = scoped.namespace().to_owned();
    let input = required_payload(request.input.clone())?;
    let span = info_span!(
        "engine_operation",
        operation = "start",
        namespace = %namespace,
        workflow_id = tracing::field::Empty,
        workflow_type = %request.workflow_type,
    );
    let search_attributes = namespace_search_attributes(&namespace);
    let handle = async {
        scoped
            .engine()
            .map_err(|error| log_server_error("start", Some(&namespace), None, &error))?
            .start_workflow(&request.workflow_type, input, search_attributes)
            .await
            .map_err(|error| map_start_error(error, &request.workflow_type))
    }
    .instrument(span.clone())
    .await?;
    span.record("workflow_id", tracing::field::display(handle.workflow_id()));

    Ok(ProtoStartWorkflowResponse {
        workflow_id: Some(handle.workflow_id().clone().into()),
        run_id: Some(handle.run_id().clone().into()),
    })
}

/// Search attribute map stamping the authorized namespace onto an execution.
fn namespace_search_attributes(
    namespace: &str,
) -> std::collections::HashMap<String, aion_core::SearchAttributeValue> {
    std::collections::HashMap::from([(
        crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
        aion_core::SearchAttributeValue::String(namespace.to_owned()),
    )])
}

/// Narrow a caller-supplied visibility filter to the authorized namespace.
///
/// The predicate is appended (predicates AND together), so a caller-supplied
/// `aion.namespace` predicate for another tenant simply matches nothing.
fn scope_visibility_filter(
    mut filter: aion_store::visibility::ListWorkflowsFilter,
    namespace: &str,
) -> aion_store::visibility::ListWorkflowsFilter {
    filter
        .search_attributes
        .push(aion_store::visibility::SearchAttributePredicate::Equals {
            name: crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
            value: aion_core::SearchAttributeValue::String(namespace.to_owned()),
        });
    filter
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
    let target = WorkflowTarget::workflow(&workflow_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::signal(&request, target))
        .await
        .map_err(|error| error.to_wire_error())?;
    let namespace = scoped.namespace().to_owned();
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    let run_id = resolve_run_id(engine.as_ref(), &workflow_id, request.run_id.clone()).await?;
    let payload = required_payload(request.payload.clone())?;
    if let Some(status) = terminal_status(engine.as_ref(), &workflow_id).await? {
        return Err(signal_terminal_error(&workflow_id, status));
    }

    let signal_name = request.signal_name.clone();
    let span = info_span!(
        "engine_operation",
        operation = "signal",
        namespace = %namespace,
        workflow_id = %workflow_id,
        signal_name = %signal_name,
    );

    async {
        engine
            .signal(&workflow_id, &run_id, signal_name, payload)
            .await
            .map_err(|error| map_workflow_operation_error(error, &workflow_id))
    }
    .instrument(span)
    .await?;

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
    let target = WorkflowTarget::workflow(&workflow_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::query(&request, target))
        .await
        .map_err(|error| error.to_wire_error())?;
    let namespace = scoped.namespace().to_owned();
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    let run_id = resolve_run_id(engine.as_ref(), &workflow_id, request.run_id.clone()).await?;
    let query_name = request.query_name.clone();
    let span = info_span!(
        "engine_operation",
        operation = "query",
        namespace = %namespace,
        workflow_id = %workflow_id,
        query_name = %query_name,
    );

    let outcome = async { engine.query(&workflow_id, &run_id, query_name).await }
        .instrument(span)
        .await;

    match outcome {
        Ok(result) => Ok(ProtoQueryResponse {
            outcome: Some(proto_query_response::Outcome::Result(result.into())),
        }),
        // Query-semantic failures (unknown query, timeout, not running,
        // handler failure, reply dropped) are the operation's documented
        // outcome and ride the QueryResponse.error oneof, which every SDK
        // query op parses. Namespace, not-found, and backend failures stay
        // transport-level errors, exactly as for every other operation.
        Err(error @ aion::EngineError::Query(_)) => Ok(ProtoQueryResponse {
            outcome: Some(proto_query_response::Outcome::Error(
                ServerError::from(error).to_wire_error().into(),
            )),
        }),
        Err(error) => Err(map_workflow_operation_error(error, &workflow_id)),
    }
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
    let target = WorkflowTarget::workflow(&workflow_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::cancel(&request, target))
        .await
        .map_err(|error| error.to_wire_error())?;
    let namespace = scoped.namespace().to_owned();
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    let run_id = resolve_run_id(engine.as_ref(), &workflow_id, request.run_id.clone()).await?;
    if let Some(status) = terminal_status(engine.as_ref(), &workflow_id).await? {
        return Err(cancel_terminal_error(&workflow_id, status));
    }

    let span = info_span!(
        "engine_operation",
        operation = "cancel",
        namespace = %namespace,
        workflow_id = %workflow_id,
    );

    async {
        engine
            .cancel(&workflow_id, &run_id, request.reason)
            .await
            .map_err(|error| map_workflow_operation_error(error, &workflow_id))
    }
    .instrument(span)
    .await?;

    Ok(ProtoCancelResponse {})
}

/// Handles a decoded list-workflows request.
///
/// The decoded filter is always narrowed to the authorized namespace via an
/// `aion.namespace` equality predicate, so a shared engine never leaks another
/// tenant's workflow summaries.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the filter envelope is malformed, namespace scoping fails, the
/// visibility-store list call fails, or summaries cannot be encoded.
pub async fn list(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoListWorkflowsRequest,
) -> Result<ProtoListWorkflowsResponse, WireError> {
    let scope_filter = WorkflowFilter::default();
    let scoped = guard
        .scope(caller, &NamespaceOperation::list(&request, &scope_filter))
        .await
        .map_err(|error| error.to_wire_error())?;
    let filter = scope_visibility_filter(
        decode_visibility_filter(request.filter.as_ref())?,
        scoped.namespace(),
    );

    let summaries = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .visibility_store()
        .list_workflows(filter)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    let namespace = scoped.namespace().to_owned();
    let summaries = summaries
        .into_iter()
        .map(|summary| encode_core_value(namespace.clone(), None, &summary))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProtoListWorkflowsResponse { summaries })
}

/// Handles a decoded count-workflows request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the filter envelope is malformed, namespace scoping fails, or
/// the visibility-store count call fails.
pub async fn count(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoCountWorkflowsRequest,
) -> Result<ProtoCountWorkflowsResponse, WireError> {
    let scoped = guard
        .scope(caller, &NamespaceOperation::count(&request))
        .await
        .map_err(|error| error.to_wire_error())?;
    let filter = scope_visibility_filter(
        decode_visibility_filter(request.filter.as_ref())?,
        scoped.namespace(),
    );

    let count = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .visibility_store()
        .count_workflows(filter)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoCountWorkflowsResponse { count })
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
    let target = WorkflowTarget::workflow(&workflow_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::describe(&request, target))
        .await
        .map_err(|error| error.to_wire_error())?;
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    resolve_run_id(engine.as_ref(), &workflow_id, request.run_id.clone()).await?;

    let history = engine
        .store()
        .read_history(&workflow_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let summary = WorkflowSummary::from_history(&history)
        .ok_or_else(|| workflow_not_found_error(&workflow_id))?;
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

async fn resolve_run_id(
    engine: &aion::Engine,
    workflow_id: &WorkflowId,
    id: Option<aion_proto::ProtoRunId>,
) -> Result<RunId, WireError> {
    if let Some(id) = id {
        return id.try_into();
    }

    let chain = engine
        .store()
        .read_run_chain(workflow_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    chain
        .last()
        .map(|summary| summary.run_id.clone())
        .ok_or_else(|| workflow_not_found_error(workflow_id))
}

fn map_start_error(error: aion::EngineError, workflow_type: &str) -> WireError {
    match error {
        aion::EngineError::WorkflowNotFound { .. } => WireError::not_found_with_type(
            "WorkflowTypeNotFound",
            format!("workflow type {workflow_type} is not registered"),
        ),
        other => ServerError::from(other).to_wire_error(),
    }
}

fn map_workflow_operation_error(error: aion::EngineError, workflow_id: &WorkflowId) -> WireError {
    match error {
        aion::EngineError::WorkflowNotFound { .. } => workflow_not_found_error(workflow_id),
        other => ServerError::from(other).to_wire_error(),
    }
}

fn workflow_not_found_error(workflow_id: &WorkflowId) -> WireError {
    WireError::not_found_with_type(
        "WorkflowNotFound",
        format!("workflow {workflow_id} not found"),
    )
}

async fn terminal_status(
    engine: &aion::Engine,
    workflow_id: &WorkflowId,
) -> Result<Option<WorkflowStatus>, WireError> {
    let history = engine
        .store()
        .read_history(workflow_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    Ok(WorkflowSummary::from_history(&history)
        .map(|summary| summary.status)
        .filter(|status| status.is_terminal()))
}

fn signal_terminal_error(workflow_id: &WorkflowId, status: WorkflowStatus) -> WireError {
    WireError::not_running_with_type(
        "WorkflowTerminal",
        format!("workflow {workflow_id} has already reached terminal state {status:?}"),
    )
}

fn cancel_terminal_error(workflow_id: &WorkflowId, status: WorkflowStatus) -> WireError {
    WireError::not_running_with_type(
        "WorkflowTerminal",
        format!("workflow {workflow_id} has already completed with status {status:?}"),
    )
}

fn required_payload(payload: Option<ProtoPayload>) -> Result<Payload, WireError> {
    payload
        .ok_or_else(|| WireError::backend("payload is missing"))?
        .try_into()
}

fn decode_visibility_filter(
    filter: Option<&aion_proto::WireEnvelope>,
) -> Result<aion_store::visibility::ListWorkflowsFilter, WireError> {
    filter.map_or_else(
        || Ok(aion_store::visibility::ListWorkflowsFilter::default()),
        decode_core_value,
    )
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

fn log_server_error(
    operation: &'static str,
    namespace: Option<&str>,
    workflow_id: Option<&WorkflowId>,
    error: &ServerError,
) -> WireError {
    let fields = error.trace_fields();
    tracing::error!(
        operation,
        namespace,
        workflow_id = workflow_id.map(ToString::to_string).as_deref(),
        error_type = %fields.error_type,
        store_error_type = fields.store_error_type,
        reason = %fields.reason,
        "request handler failed"
    );
    error.to_wire_error()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion::{Engine, EngineBuilder};
    use aion_core::{Event, EventEnvelope, Payload, WorkflowStatus};
    use aion_proto::{
        WireErrorCode,
        convert::{decode_core_value, decode_event, decode_workflow_summary, encode_core_value},
    };
    use aion_store::{
        EventStore, InMemoryStore, WriteToken,
        visibility::{VisibilityRecord, VisibilityStore},
    };
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::NamespaceMode,
    };

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

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowTypeNotFound"));
        assert_eq!(
            error.message,
            "workflow type missing-workflow is not registered"
        );
        Ok(())
    }

    #[tokio::test]
    async fn signal_handler_scopes_then_invokes_engine_signal()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = signal(&context.guard, &context.caller, signal_request()?).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowNotFound"));
        assert_eq!(
            error.message,
            format!("workflow {} not found", workflow_id())
        );
        Ok(())
    }

    #[tokio::test]
    async fn query_handler_scopes_then_invokes_engine_query()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = query(&context.guard, &context.caller, query_request()).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowNotFound"));
        assert_eq!(
            error.message,
            format!("workflow {} not found", workflow_id())
        );
        Ok(())
    }

    #[tokio::test]
    async fn query_handler_returns_not_running_outcome_for_terminal_workflow()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_completed(context.store.as_ref()).await?;
        // Resolve the latest run from the chain: the completed history was
        // recorded for the started run, not the fixed test run id.
        let mut request = query_request();
        request.run_id = None;

        let response = query(&context.guard, &context.caller, request).await?;

        // A terminal workflow is a query-semantic outcome: the transport call
        // succeeds and the typed error rides the QueryResponse.error oneof.
        let Some(proto_query_response::Outcome::Error(error)) = response.outcome else {
            return Err("expected a QueryResponse.error outcome".into());
        };
        let error = WireError::try_from(error)?;
        assert_eq!(error.code, WireErrorCode::NotRunning);
        assert_eq!(error.error_type.as_deref(), Some("QueryNotRunning"));
        Ok(())
    }

    #[tokio::test]
    async fn query_handler_keeps_non_resident_non_terminal_workflow_as_transport_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        // A recorded but non-resident, non-terminal workflow misses the live
        // registry and has no terminal history, so Engine::query reports
        // WorkflowNotFound — a transport-level error, never an outcome.error.
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_started(context.store.as_ref()).await?;
        let mut request = query_request();
        request.run_id = None;

        let error = query(&context.guard, &context.caller, request).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowNotFound"));
        Ok(())
    }

    #[tokio::test]
    async fn cancel_handler_scopes_then_invokes_engine_cancel()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = cancel(&context.guard, &context.caller, cancel_request()).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowNotFound"));
        assert_eq!(
            error.message,
            format!("workflow {} not found", workflow_id())
        );
        Ok(())
    }

    #[tokio::test]
    async fn signal_handler_rejects_completed_workflow() -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_completed(context.store.as_ref()).await?;

        let error = signal(&context.guard, &context.caller, signal_request()?).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotRunning);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowTerminal"));
        assert_eq!(
            error.message,
            format!(
                "workflow {} has already reached terminal state Completed",
                workflow_id()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn signal_handler_rejects_failed_workflow() -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_failed(context.store.as_ref()).await?;

        let error = signal(&context.guard, &context.caller, signal_request()?).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotRunning);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowTerminal"));
        assert_eq!(
            error.message,
            format!(
                "workflow {} has already reached terminal state Failed",
                workflow_id()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancel_handler_rejects_completed_workflow() -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_completed(context.store.as_ref()).await?;

        let error = cancel(&context.guard, &context.caller, cancel_request()).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotRunning);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowTerminal"));
        assert_eq!(
            error.message,
            format!(
                "workflow {} has already completed with status Completed",
                workflow_id()
            )
        );
        assert!(!error.message.contains("process 0 is not live"));
        Ok(())
    }

    #[tokio::test]
    async fn cancel_handler_rejects_failed_workflow() -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_failed(context.store.as_ref()).await?;

        let error = cancel(&context.guard, &context.caller, cancel_request()).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotRunning);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowTerminal"));
        assert_eq!(
            error.message,
            format!(
                "workflow {} has already completed with status Failed",
                workflow_id()
            )
        );
        assert!(!error.message.contains("process 0 is not live"));
        Ok(())
    }

    #[tokio::test]
    async fn list_handler_scopes_then_invokes_engine_list() -> Result<(), Box<dyn std::error::Error>>
    {
        let context = context().await?;
        append_started(context.store.as_ref()).await?;
        context
            .visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: run_id(),
                workflow_type: String::from("fixture"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: std::collections::HashMap::from([(
                    crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
                    aion_core::SearchAttributeValue::String(NAMESPACE.to_owned()),
                )]),
            })
            .await?;
        let request = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(encode_core_value(
                NAMESPACE,
                None,
                &aion_store::visibility::ListWorkflowsFilter {
                    workflow_type: Some(String::from("fixture")),
                    status: Some(WorkflowStatus::Running),
                    ..aion_store::visibility::ListWorkflowsFilter::default()
                },
            )?),
        };

        let response = list(&context.guard, &context.caller, request).await?;

        assert_eq!(response.summaries.len(), 1);
        let summary =
            decode_core_value::<aion_store::visibility::WorkflowSummary>(&response.summaries[0])?;
        assert_eq!(summary.workflow_id, workflow_id());
        Ok(())
    }

    #[tokio::test]
    async fn describe_handler_scopes_then_reads_summary_and_optional_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_started(context.store.as_ref()).await?;

        let response = describe(
            &context.guard,
            &context.caller,
            describe_request(true, None),
        )
        .await?;

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
    async fn omitted_run_id_resolves_latest_run_from_chain()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let first = RunId::new(uuid::Uuid::from_u128(11));
        let latest = RunId::new(uuid::Uuid::from_u128(12));
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_continued_chain(context.store.as_ref(), &first, &latest).await?;

        let engine = context
            .guard
            .scope(
                &context.caller,
                &NamespaceOperation::describe(
                    &describe_request(false, None),
                    WorkflowTarget::workflow(&workflow_id()),
                ),
            )
            .await?;
        let resolved = resolve_run_id(engine.engine()?.as_ref(), &workflow_id(), None).await?;

        assert_eq!(resolved, latest);
        Ok(())
    }

    #[tokio::test]
    async fn supplied_run_id_takes_precedence_over_latest_chain_entry()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let requested = RunId::new(uuid::Uuid::from_u128(10));
        let latest = RunId::new(uuid::Uuid::from_u128(12));
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_continued_chain(context.store.as_ref(), &requested, &latest).await?;

        let engine = context
            .guard
            .scope(
                &context.caller,
                &NamespaceOperation::describe(
                    &describe_request(false, Some(requested.clone())),
                    WorkflowTarget::workflow(&workflow_id()),
                ),
            )
            .await?;
        let resolved = resolve_run_id(
            engine.engine()?.as_ref(),
            &workflow_id(),
            Some(requested.clone().into()),
        )
        .await?;

        assert_eq!(resolved, requested);
        Ok(())
    }

    #[tokio::test]
    async fn describe_handler_maps_omitted_run_missing_workflow_to_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = describe(
            &context.guard,
            &context.caller,
            describe_request(false, None),
        )
        .await;

        assert_workflow_not_found(error)?;
        Ok(())
    }

    #[tokio::test]
    async fn describe_handler_maps_empty_history_to_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = describe(
            &context.guard,
            &context.caller,
            describe_request(false, Some(run_id())),
        )
        .await;

        assert_workflow_not_found(error)?;
        Ok(())
    }

    #[tokio::test]
    async fn signal_handler_maps_omitted_run_missing_workflow_to_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        let mut request = signal_request()?;
        request.run_id = None;

        let error = signal(&context.guard, &context.caller, request).await;

        assert_workflow_not_found(error)?;
        Ok(())
    }

    #[tokio::test]
    async fn query_handler_maps_omitted_run_missing_workflow_to_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        let mut request = query_request();
        request.run_id = None;

        let error = query(&context.guard, &context.caller, request).await;

        assert_workflow_not_found(error)?;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_handler_maps_omitted_run_missing_workflow_to_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        let mut request = cancel_request();
        request.run_id = None;

        let error = cancel(&context.guard, &context.caller, request).await;

        assert_workflow_not_found(error)?;
        Ok(())
    }

    fn assert_workflow_not_found<T>(result: Result<T, WireError>) -> Result<(), WireError> {
        let error = result
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowNotFound"));
        assert_eq!(
            error.message,
            format!("workflow {} not found", workflow_id())
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_handler_returns_namespace_denied_before_engine_access()
    -> Result<(), Box<dyn std::error::Error>> {
        let ownership = StaticWorkflowNamespaces::default();
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );
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

    #[tokio::test]
    async fn denied_signal_does_not_decode_missing_payload_before_namespace_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let (guard, caller) = denied_guard();
        let request = ProtoSignalRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(workflow_id().into()),
            run_id: Some(run_id().into()),
            signal_name: "poke".to_owned(),
            payload: None,
        };

        let error = signal(&guard, &caller, request).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NamespaceDenied)
        );
        Ok(())
    }

    struct TestContext {
        guard: NamespaceGuard,
        caller: CallerIdentity,
        ownership: StaticWorkflowNamespaces,
        store: Arc<dyn EventStore>,
        visibility_store: Arc<dyn VisibilityStore>,
    }

    async fn context() -> Result<TestContext, aion::EngineError> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = backing.clone();
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(Arc::clone(&store))
                .visibility_store_arc(Arc::clone(&visibility_store))
                .scheduler_threads(1)
                .build()
                .await?,
        );
        Ok(context_from_engine(engine, store, visibility_store))
    }

    fn context_from_engine(
        engine: Arc<Engine>,
        store: Arc<dyn EventStore>,
        visibility_store: Arc<dyn VisibilityStore>,
    ) -> TestContext {
        let ownership = StaticWorkflowNamespaces::default();
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(ownership.clone()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        TestContext {
            guard: NamespaceGuard::new(resolver),
            caller: CallerIdentity::new("alice", [NAMESPACE.to_owned()]),
            ownership,
            store,
            visibility_store,
        }
    }

    fn denied_guard() -> (NamespaceGuard, CallerIdentity) {
        let ownership = StaticWorkflowNamespaces::default();
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );
        let guard = NamespaceGuard::new(resolver);
        let caller = CallerIdentity::new("alice", [String::from("tenant-b")]);
        (guard, caller)
    }

    async fn append_started(store: &dyn EventStore) -> Result<(), Box<dyn std::error::Error>> {
        let event = started_event()?;
        store
            .append(WriteToken::recorder(), &workflow_id(), &[event], 0)
            .await?;
        Ok(())
    }

    async fn append_completed(store: &dyn EventStore) -> Result<(), Box<dyn std::error::Error>> {
        let events = [
            started_event()?,
            Event::WorkflowCompleted {
                envelope: event_envelope(2),
                result: payload()?,
            },
        ];
        store
            .append(WriteToken::recorder(), &workflow_id(), &events, 0)
            .await?;
        Ok(())
    }

    async fn append_failed(store: &dyn EventStore) -> Result<(), Box<dyn std::error::Error>> {
        let events = [
            started_event()?,
            Event::WorkflowFailed {
                envelope: event_envelope(2),
                error: aion_core::WorkflowError {
                    message: "fixture failure".to_owned(),
                    details: None,
                },
            },
        ];
        store
            .append(WriteToken::recorder(), &workflow_id(), &events, 0)
            .await?;
        Ok(())
    }

    async fn append_continued_chain(
        store: &dyn EventStore,
        first: &RunId,
        latest: &RunId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let events = [
            Event::WorkflowStarted {
                envelope: event_envelope(1),
                workflow_type: "fixture".to_owned(),
                input: payload()?,
                run_id: first.clone(),
                parent_run_id: None,
            },
            Event::WorkflowContinuedAsNew {
                envelope: event_envelope(2),
                input: payload()?,
                workflow_type: None,
                parent_run_id: first.clone(),
            },
            Event::WorkflowStarted {
                envelope: event_envelope(3),
                workflow_type: "fixture".to_owned(),
                input: payload()?,
                run_id: latest.clone(),
                parent_run_id: Some(first.clone()),
            },
        ];
        store
            .append(WriteToken::recorder(), &workflow_id(), &events, 0)
            .await?;
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

    fn describe_request(
        include_history: bool,
        run_id: Option<RunId>,
    ) -> ProtoDescribeWorkflowRequest {
        ProtoDescribeWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(workflow_id().into()),
            run_id: run_id.map(Into::into),
            include_history,
        }
    }

    fn started_event() -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: event_envelope(1),
            workflow_type: "fixture".to_owned(),
            input: payload()?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
        })
    }

    fn event_envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: workflow_id(),
        }
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
