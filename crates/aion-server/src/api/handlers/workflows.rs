//! Start/signal/query/cancel workflow operation handlers.

use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoQueryRequest, ProtoQueryResponse,
    ProtoReopenRequest, ProtoReopenResponse, ProtoSignalRequest, ProtoSignalResponse,
    ProtoStartWorkflowRequest, ProtoStartWorkflowResponse, WireError, proto_query_response,
};
use tracing::{Instrument, info_span};

use super::error::{
    cancel_terminal_error, log_server_error, map_start_error, map_workflow_operation_error,
    signal_terminal_error,
};
use super::payload::{required_payload, required_workflow_id};
use super::runs::{resolve_run_id, terminal_status};
use crate::{
    CallerIdentity, NamespaceGuard, NamespaceMinter, NamespaceOperation, ServerError,
    WorkflowTarget,
};

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
    start_with_placement(guard, caller, request, None, None).await
}

/// Start a workflow, optionally with a `placement` id chosen by the routing edge
/// so the new execution lands on a locally-owned shard (R-1 unsteered-start
/// remint). `placement = None` is the default path: the engine mints the id, so
/// the single-node / non-clustered behaviour is unchanged.
///
/// `minter` is the minted-on-use safety net (Control-Plane Phase 1, S6): when
/// `Some`, the resolved-and-authorized namespace is durably minted (open) or
/// gated (closed) BEFORE the engine start, so a client that starts a workflow
/// before any worker registers still gets a durable namespace record. It is the
/// SAME [`NamespaceMinter`] policy the worker-registration seam (S5) applies, so
/// the two transports and the two mint choke-points can never diverge. `None`
/// disables the mint entirely (every unit test of the bare handler), leaving the
/// start path byte-identical.
///
/// The mint runs AFTER namespace authorization (`guard.scope`), so it is
/// auth-scoped by construction — it can only record a namespace the caller is
/// already permitted to start in. It does NOT change the immutable NSTQ
/// `aion.namespace` binding ([`start_search_attributes`]) or the start response
/// shape; the mint is purely additive.
///
/// # Errors
///
/// Identical to [`start`], plus a durable-store failure (a retryable `NotOwner`
/// fence surfaces as such) or a `closed`-policy namespace-denied error from the
/// minter, all mapped to a stable [`WireError`].
pub async fn start_with_placement(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoStartWorkflowRequest,
    placement: Option<aion_core::WorkflowId>,
    minter: Option<&NamespaceMinter>,
) -> Result<ProtoStartWorkflowResponse, WireError> {
    let scoped = guard
        .scope(caller, &NamespaceOperation::start(&request))
        .await
        .map_err(|error| error.to_wire_error())?;
    let namespace = scoped.namespace().to_owned();
    // MINT-ON-START safety net (Phase 1 S6). Runs strictly AFTER the namespace
    // authorization above, so it can only ever mint a namespace the caller is
    // already authorized to start in — auth-scoped by construction. A `closed`
    // policy rejects an unknown namespace with the same namespace-denied error;
    // a quorum `NotOwner` fence propagates as the retryable wire code, never a
    // silent success. Shares the EXACT S5 policy via `NamespaceMinter`.
    if let Some(minter) = minter {
        minter
            .mint_or_gate(
                std::slice::from_ref(&namespace),
                aion_store::NamespaceOrigin::StartMint,
            )
            .await
            .map_err(|error| error.to_wire_error())?;
    }
    let input = required_payload(request.input.clone())?;
    // An empty task_queue means "not selected": fall back to the namespace's
    // default queue rather than recording an empty selection.
    let task_queue = request
        .task_queue
        .as_deref()
        .map(str::trim)
        .filter(|queue| !queue.is_empty());
    let span = info_span!(
        "engine_operation",
        operation = "start",
        namespace = %namespace,
        workflow_id = tracing::field::Empty,
        workflow_type = %request.workflow_type,
    );
    let search_attributes = start_search_attributes(&namespace, task_queue);
    let handle = async {
        scoped
            .engine()
            .map_err(|error| log_server_error("start", Some(&namespace), None, &error))?
            .start_workflow_with_id(
                &request.workflow_type,
                input,
                search_attributes,
                namespace.clone(),
                placement,
                // Steered-start shard derivation already happened at the edge
                // (which holds the concrete cluster store); the engine receives
                // the derived placement id, so no routing key is threaded here.
                None,
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

/// Search attribute map stamping the authorized namespace — and, when the start
/// selected one, the default task queue — onto an execution.
///
/// Both are recorded in the same atomic append as `WorkflowStarted`, so the
/// `(namespace, task_queue)` targeting selection survives restarts/failover and
/// is never tracked only in memory. `task_queue` is omitted when the start did
/// not select one (the workflow falls back to the namespace's default queue).
fn start_search_attributes(
    namespace: &str,
    task_queue: Option<&str>,
) -> std::collections::HashMap<String, aion_core::SearchAttributeValue> {
    let mut attributes = std::collections::HashMap::from([(
        crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
        aion_core::SearchAttributeValue::String(namespace.to_owned()),
    )]);
    if let Some(task_queue) = task_queue {
        attributes.insert(
            crate::namespace::TASK_QUEUE_ATTRIBUTE.to_owned(),
            aion_core::SearchAttributeValue::String(task_queue.to_owned()),
        );
    }
    attributes
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

/// Handles a decoded reopen request.
///
/// Resolves the run (latest when omitted) and calls
/// [`aion::Engine::reopen_workflow`], returning the reopened run id and its
/// projected Running status. UNLIKE [`cancel`] this does NOT pre-check terminal
/// status: the terminal-reopenable precondition is the engine's (AD-012) and the
/// handler only surfaces its typed [`aion::EngineError::InvalidState`] error.
///
/// # Errors
///
/// Returns a stable [`WireError`] when IDs are missing or malformed, namespace
/// scoping fails, or the engine reopen call fails — `invalid_state` for a
/// non-reopenable-terminal run, `not_found` for an absent workflow.
pub async fn reopen(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoReopenRequest,
) -> Result<ProtoReopenResponse, WireError> {
    let workflow_id = required_workflow_id(request.workflow_id.clone())?;
    let target = WorkflowTarget::workflow(&workflow_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::reopen(&request, target))
        .await
        .map_err(|error| error.to_wire_error())?;
    let namespace = scoped.namespace().to_owned();
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    let run_id = resolve_run_id(engine.as_ref(), &workflow_id, request.run_id.clone()).await?;

    let span = info_span!(
        "engine_operation",
        operation = "reopen",
        namespace = %namespace,
        workflow_id = %workflow_id,
    );

    let handle = async {
        engine
            .reopen_workflow(&workflow_id, &run_id)
            .await
            .map_err(|error| map_workflow_operation_error(error, &workflow_id))
    }
    .instrument(span)
    .await?;

    Ok(ProtoReopenResponse {
        run_id: Some(handle.run_id().clone().into()),
        status: aion_proto::ProtoWorkflowStatus::from(handle.cached_status()) as i32,
    })
}

#[cfg(test)]
mod tests {
    use aion_proto::{WireError, WireErrorCode};

    use super::super::test_support::{
        NAMESPACE, append_completed, append_failed, append_started, append_timed_out,
        assert_workflow_not_found, cancel_request, context, denied_guard, proto_payload,
        query_request, reopen_request, run_id, signal_request, workflow_id,
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
            routing_key: None,
            task_queue: None,
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

    #[test]
    fn start_records_namespace_only_when_no_task_queue_selected() {
        use crate::namespace::{NAMESPACE_ATTRIBUTE, TASK_QUEUE_ATTRIBUTE};

        let attributes = start_search_attributes("tenant-a", None);
        assert_eq!(
            attributes.get(NAMESPACE_ATTRIBUTE),
            Some(&aion_core::SearchAttributeValue::String(
                "tenant-a".to_owned()
            ))
        );
        // No selection => no task_queue attribute is recorded, so the workflow
        // falls back to the namespace's default queue.
        assert!(!attributes.contains_key(TASK_QUEUE_ATTRIBUTE));
    }

    #[test]
    fn start_records_selected_task_queue_durably_like_namespace() {
        use crate::namespace::{NAMESPACE_ATTRIBUTE, TASK_QUEUE_ATTRIBUTE};

        let attributes = start_search_attributes("tenant-a", Some("gpu"));
        assert_eq!(
            attributes.get(NAMESPACE_ATTRIBUTE),
            Some(&aion_core::SearchAttributeValue::String(
                "tenant-a".to_owned()
            ))
        );
        // The selected task_queue rides the SAME search-attribute map as the
        // namespace, so it lands in the same atomic WorkflowStarted append and
        // survives replay/failover exactly as the namespace does.
        assert_eq!(
            attributes.get(TASK_QUEUE_ATTRIBUTE),
            Some(&aion_core::SearchAttributeValue::String("gpu".to_owned()))
        );
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
    async fn reopen_handler_maps_missing_workflow_to_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;

        let error = reopen(&context.guard, &context.caller, reopen_request()).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowNotFound"));
        Ok(())
    }

    #[tokio::test]
    async fn reopen_handler_rejects_completed_workflow_as_invalid_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_completed(context.store.as_ref()).await?;
        let mut request = reopen_request();
        request.run_id = None;

        let error = reopen(&context.guard, &context.caller, request).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(error.code, WireErrorCode::InvalidState);
        assert_eq!(error.error_type.as_deref(), Some("InvalidState"));
        Ok(())
    }

    #[tokio::test]
    async fn reopen_handler_rejects_timed_out_workflow_as_invalid_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_timed_out(context.store.as_ref()).await?;
        let mut request = reopen_request();
        request.run_id = None;

        let error = reopen(&context.guard, &context.caller, request).await;

        let error = error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        // TimedOut is a non-reopenable terminal (only Failed and Cancelled reopen).
        assert_eq!(error.code, WireErrorCode::InvalidState);
        assert_eq!(error.error_type.as_deref(), Some("InvalidState"));
        Ok(())
    }

    #[tokio::test]
    async fn reopen_handler_maps_omitted_run_missing_workflow_to_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        context.ownership.record(workflow_id(), NAMESPACE)?;
        let mut request = reopen_request();
        request.run_id = None;

        let error = reopen(&context.guard, &context.caller, request).await;

        assert_workflow_not_found(error)?;
        Ok(())
    }

    /// A caller WITHOUT a grant for the target namespace is denied reopen with
    /// the namespace-denied wire code — mirroring the signal denial test.
    #[tokio::test]
    async fn denied_reopen_is_namespace_denied_before_engine_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let (guard, caller) = denied_guard();
        let request = ProtoReopenRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(workflow_id().into()),
            run_id: Some(run_id().into()),
        };

        let error = reopen(&guard, &caller, request).await;

        assert_eq!(
            error.err().map(|error| error.code),
            Some(WireErrorCode::NamespaceDenied)
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
            routing_key: None,
            task_queue: None,
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

    // ---- Minted-on-use START safety net (Control-Plane Phase 1, S6) --------

    use std::sync::Arc;

    use aion_store::{NamespaceOrigin, NamespaceStore};

    use crate::config::AutoCreate;

    fn namespace_store() -> Arc<dyn NamespaceStore> {
        Arc::new(aion_store::InMemoryStore::default())
    }

    fn minter(store: &Arc<dyn NamespaceStore>, policy: AutoCreate) -> NamespaceMinter {
        NamespaceMinter::new(Arc::clone(store), policy)
    }

    fn fresh_start_request() -> Result<ProtoStartWorkflowRequest, aion_core::PayloadError> {
        Ok(ProtoStartWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_type: "missing-workflow".to_owned(),
            input: Some(proto_payload()?),
            routing_key: None,
            task_queue: None,
        })
    }

    /// A start into a never-before-seen namespace (no worker registered) mints a
    /// durable record under the open policy, even though the start itself fails
    /// at the engine (no such workflow type) — the mint runs strictly after
    /// authorization and before the engine call. A second start is idempotent:
    /// no duplicate row.
    #[tokio::test]
    async fn open_start_mints_durable_record_and_is_idempotent()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let store = namespace_store();
        let minter = minter(&store, AutoCreate::Open);

        // No worker ever registered, so the namespace has no row yet.
        assert!(store.get_namespace(NAMESPACE).await?.is_none());

        // The start fails at the engine (unknown workflow type) but the mint
        // already ran: a durable record exists afterwards.
        let first = start_with_placement(
            &context.guard,
            &context.caller,
            fresh_start_request()?,
            None,
            Some(&minter),
        )
        .await;
        assert!(
            first.is_err(),
            "the fixture start has no registered workflow type"
        );
        let record = store
            .get_namespace(NAMESPACE)
            .await?
            .ok_or("expected a durable record minted by the start")?;
        assert_eq!(record.name, NAMESPACE);
        assert_eq!(record.origin, NamespaceOrigin::StartMint);

        // A second start is idempotent: still exactly one row, no duplicate.
        let _second = start_with_placement(
            &context.guard,
            &context.caller,
            fresh_start_request()?,
            None,
            Some(&minter),
        )
        .await;
        let all = store.list_namespaces().await?;
        assert_eq!(
            all.iter().filter(|r| r.name == NAMESPACE).count(),
            1,
            "a second start must not create a duplicate namespace row"
        );
        Ok(())
    }

    /// Under the closed policy a start into an unknown namespace is rejected with
    /// the same namespace-denied error the worker-registration seam uses, and the
    /// namespace is not created.
    #[tokio::test]
    async fn closed_start_rejects_unknown_namespace_and_does_not_create_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let store = namespace_store();
        let minter = minter(&store, AutoCreate::Closed);

        let denied = start_with_placement(
            &context.guard,
            &context.caller,
            fresh_start_request()?,
            None,
            Some(&minter),
        )
        .await;

        let error = denied
            .err()
            .ok_or_else(|| WireError::backend("expected a namespace-denied error"))?;
        assert_eq!(error.code, WireErrorCode::NamespaceDenied);
        assert!(
            store.get_namespace(NAMESPACE).await?.is_none(),
            "closed policy must NOT create the namespace it rejected"
        );
        Ok(())
    }

    /// Under the closed policy a start into a namespace that already has a
    /// durable record (the `POST /namespaces` escape hatch's effect) is admitted
    /// — it proceeds to the engine exactly as the open path does.
    #[tokio::test]
    async fn closed_start_admits_a_known_namespace() -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let store = namespace_store();
        store
            .register_namespace(NAMESPACE, NamespaceOrigin::Explicit)
            .await?;
        let minter = minter(&store, AutoCreate::Closed);

        // The known namespace passes the gate, so the start reaches the engine
        // and fails only on the unknown workflow type — never on the namespace.
        let error = start_with_placement(
            &context.guard,
            &context.caller,
            fresh_start_request()?,
            None,
            Some(&minter),
        )
        .await
        .err()
        .ok_or_else(|| WireError::backend("expected the fixture workflow-type miss"))?;
        assert_eq!(
            error.code,
            WireErrorCode::NotFound,
            "a known namespace must pass the gate and fail only at the engine"
        );
        assert_eq!(error.error_type.as_deref(), Some("WorkflowTypeNotFound"));
        Ok(())
    }

    /// With no minter installed the start path is byte-identical to before S6:
    /// the namespace is never touched and the start reaches the engine as usual.
    #[tokio::test]
    async fn no_minter_leaves_start_untouched() -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let error = start_with_placement(
            &context.guard,
            &context.caller,
            fresh_start_request()?,
            None,
            None,
        )
        .await
        .err()
        .ok_or_else(|| WireError::backend("expected the fixture workflow-type miss"))?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowTypeNotFound"));
        Ok(())
    }
}
