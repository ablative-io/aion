//! Start/signal/query/cancel workflow operation handlers.

use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoQueryRequest, ProtoQueryResponse,
    ProtoSignalRequest, ProtoSignalResponse, ProtoStartWorkflowRequest, ProtoStartWorkflowResponse,
    WireError, proto_query_response,
};
use tracing::{Instrument, info_span};

use super::error::{
    cancel_terminal_error, log_server_error, map_start_error, map_workflow_operation_error,
    signal_terminal_error,
};
use super::payload::{required_payload, required_workflow_id};
use super::runs::{resolve_run_id, terminal_status};
use crate::{CallerIdentity, NamespaceGuard, NamespaceOperation, ServerError, WorkflowTarget};

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
            .start_workflow(
                &request.workflow_type,
                input,
                search_attributes,
                namespace.clone(),
            )
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

#[cfg(test)]
mod tests {
    use aion_proto::{WireError, WireErrorCode};

    use super::super::test_support::{
        NAMESPACE, append_completed, append_failed, append_started, assert_workflow_not_found,
        cancel_request, context, denied_guard, proto_payload, query_request, run_id,
        signal_request, workflow_id,
    };
    use super::*;

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
}
