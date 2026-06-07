//! shared handler layer over Engine

use aion_core::{Payload, RunId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoCountWorkflowsRequest,
    ProtoCountWorkflowsResponse, ProtoCreateScheduleRequest, ProtoCreateScheduleResponse,
    ProtoDeleteScheduleResponse, ProtoDescribeScheduleResponse, ProtoDescribeWorkflowRequest,
    ProtoDescribeWorkflowResponse, ProtoListSchedulesRequest, ProtoListSchedulesResponse,
    ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoPauseScheduleResponse,
    ProtoQueryRequest, ProtoQueryResponse, ProtoResumeScheduleResponse, ProtoScheduleIdRequest,
    ProtoSignalRequest, ProtoSignalResponse, ProtoStartWorkflowRequest, ProtoStartWorkflowResponse,
    ProtoUpdateScheduleRequest, ProtoUpdateScheduleResponse, WireError,
    convert::{
        ProtoPayload, ProtoScheduleId, decode_core_value, decode_schedule_config,
        encode_core_value, encode_event, encode_schedule_state, encode_workflow_summary,
    },
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
    let target = WorkflowTarget::with_run(&workflow_id, &run_id);
    let scoped = guard
        .scope(caller, &NamespaceOperation::signal(&request, target))
        .map_err(|error| error.to_wire_error())?;
    let payload = required_payload(request.payload.clone())?;

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
/// visibility-store list call fails, or summaries cannot be encoded.
pub async fn list(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoListWorkflowsRequest,
) -> Result<ProtoListWorkflowsResponse, WireError> {
    let scope_filter = WorkflowFilter::default();
    let scoped = guard
        .scope(caller, &NamespaceOperation::list(&request, &scope_filter))
        .map_err(|error| error.to_wire_error())?;
    let filter = decode_visibility_filter(request.filter.as_ref())?;

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
        .map_err(|error| error.to_wire_error())?;
    let filter = decode_visibility_filter(request.filter.as_ref())?;

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

/// Handles a decoded create-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule config is missing or malformed, namespace
/// scoping fails, or the engine create/describe call fails.
pub async fn create_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoCreateScheduleRequest,
) -> Result<ProtoCreateScheduleResponse, WireError> {
    let scoped = guard
        .scope(caller, &NamespaceOperation::create_schedule(&request))
        .map_err(|error| error.to_wire_error())?;
    let config = required_schedule_config(request.config.as_ref())?;
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    let schedule_id = engine
        .create_schedule(config)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let state = engine
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoCreateScheduleResponse {
        schedule_id: Some(schedule_id.into()),
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

/// Handles a decoded update-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID or config is missing or malformed, namespace
/// scoping fails, or the engine update/describe call fails.
pub async fn update_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoUpdateScheduleRequest,
) -> Result<ProtoUpdateScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let scoped = guard
        .scope(caller, &NamespaceOperation::update_schedule(&request))
        .map_err(|error| error.to_wire_error())?;
    let config = required_schedule_config(request.config.as_ref())?;
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    engine
        .update_schedule(&schedule_id, config)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let state = engine
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoUpdateScheduleResponse {
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

/// Handles a decoded pause-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID is missing or malformed, namespace scoping
/// fails, or the engine pause/describe call fails.
pub async fn pause_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoScheduleIdRequest,
) -> Result<ProtoPauseScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let scoped = guard
        .scope(caller, &NamespaceOperation::pause_schedule(&request))
        .map_err(|error| error.to_wire_error())?;
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    engine
        .pause_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let state = engine
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoPauseScheduleResponse {
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

/// Handles a decoded resume-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID is missing or malformed, namespace scoping
/// fails, or the engine resume/describe call fails.
pub async fn resume_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoScheduleIdRequest,
) -> Result<ProtoResumeScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let scoped = guard
        .scope(caller, &NamespaceOperation::resume_schedule(&request))
        .map_err(|error| error.to_wire_error())?;
    let engine = scoped.engine().map_err(|error| error.to_wire_error())?;
    engine
        .resume_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    let state = engine
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoResumeScheduleResponse {
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
    })
}

/// Handles a decoded delete-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID is missing or malformed, namespace scoping
/// fails, or the engine delete call fails.
pub async fn delete_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoScheduleIdRequest,
) -> Result<ProtoDeleteScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let scoped = guard
        .scope(caller, &NamespaceOperation::delete_schedule(&request))
        .map_err(|error| error.to_wire_error())?;
    scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .delete_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    Ok(ProtoDeleteScheduleResponse {})
}

/// Handles a decoded list-schedules request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when namespace scoping fails, the engine list call fails, or
/// schedule states cannot be encoded.
pub async fn list_schedules(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoListSchedulesRequest,
) -> Result<ProtoListSchedulesResponse, WireError> {
    let scoped = guard
        .scope(caller, &NamespaceOperation::list_schedules(&request))
        .map_err(|error| error.to_wire_error())?;
    let namespace = scoped.namespace().to_owned();
    let schedules = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .list_schedules()
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?
        .into_iter()
        .map(|state| encode_schedule_state(namespace.clone(), None, &state))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProtoListSchedulesResponse { schedules })
}

/// Handles a decoded describe-schedule request.
///
/// # Errors
///
/// Returns a stable [`WireError`] when the schedule ID is missing or malformed, namespace scoping
/// fails, or the engine describe call fails.
pub async fn describe_schedule(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: ProtoScheduleIdRequest,
) -> Result<ProtoDescribeScheduleResponse, WireError> {
    let schedule_id = required_schedule_id(request.schedule_id.clone())?;
    let scoped = guard
        .scope(caller, &NamespaceOperation::describe_schedule(&request))
        .map_err(|error| error.to_wire_error())?;
    let state = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .describe_schedule(&schedule_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoDescribeScheduleResponse {
        state: Some(encode_schedule_state(
            scoped.namespace().to_owned(),
            None,
            &state,
        )?),
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

fn decode_visibility_filter(
    filter: Option<&aion_proto::WireEnvelope>,
) -> Result<aion_store::visibility::ListWorkflowsFilter, WireError> {
    filter.map_or_else(
        || Ok(aion_store::visibility::ListWorkflowsFilter::default()),
        decode_core_value,
    )
}

fn required_schedule_id(id: Option<ProtoScheduleId>) -> Result<aion_core::ScheduleId, WireError> {
    id.ok_or_else(|| WireError::invalid_input("schedule id is missing"))?
        .try_into()
}

fn required_schedule_config(
    config: Option<&aion_proto::WireEnvelope>,
) -> Result<aion_core::ScheduleConfig, WireError> {
    config
        .ok_or_else(|| WireError::invalid_input("schedule config is missing"))
        .and_then(decode_schedule_config)
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
        convert::{decode_core_value, decode_event, decode_workflow_summary, encode_core_value},
    };
    use aion_store::{
        EventStore, InMemoryStore,
        visibility::{VisibilityRecord, VisibilityStore},
    };
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
            Some(WireErrorCode::NotFound)
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
        context
            .visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: run_id(),
                workflow_type: String::from("fixture"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: std::collections::HashMap::new(),
            })
            .await?;
        let request = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(encode_core_value(
                NAMESPACE,
                None,
                &aion_store::visibility::ListWorkflowsFilter {
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
        ownership: WorkflowOwnership,
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
            visibility_store,
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
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
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
