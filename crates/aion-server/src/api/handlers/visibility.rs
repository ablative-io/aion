//! List/count handlers and visibility-filter namespace scoping.

use aion_core::WorkflowFilter;
use aion_proto::{
    ProtoCountWorkflowsRequest, ProtoCountWorkflowsResponse, ProtoListWorkflowsRequest,
    ProtoListWorkflowsResponse, WireError, convert::encode_core_value,
};

use super::payload::decode_visibility_filter;
use crate::{CallerIdentity, NamespaceGuard, NamespaceOperation, ServerError};

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

    let mut summaries = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .visibility_store()
        .list_workflows(filter)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    crate::internal_workflow::retain_user_workflows(&mut summaries);

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

    let visibility_store = scoped
        .engine()
        .map_err(|error| error.to_wire_error())?
        .visibility_store();
    let count = crate::internal_workflow::count_user_workflows(&visibility_store, filter)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;

    Ok(ProtoCountWorkflowsResponse { count })
}

#[cfg(test)]
mod tests {
    use aion_core::{RunId, WorkflowId, WorkflowStatus};
    use aion_proto::{
        WireErrorCode,
        convert::{ProtoPayload, decode_core_value, encode_core_value},
    };
    use aion_store::visibility::VisibilityRecord;
    use chrono::Utc;

    use super::super::test_support::{
        NAMESPACE, append_started, context, denied_guard, run_id, workflow_id,
    };
    use super::*;
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::NamespaceMode,
    };

    /// Regression test (#51): the engine's internal schedule-coordinator
    /// workflow must never surface through the shared list/count handlers
    /// (the gRPC list/count RPCs and `POST /workflows/list` ride these).
    /// The coordinator record carries the tenant namespace attribute here to
    /// model any path that scopes the coordinator into a tenant — namespace
    /// scoping must not be the only thing hiding engine internals.
    #[tokio::test]
    async fn list_and_count_handlers_hide_engine_internal_workflows()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        append_started(context.store.as_ref()).await?;
        let namespace_attributes = std::collections::HashMap::from([(
            crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
            aion_core::SearchAttributeValue::String(NAMESPACE.to_owned()),
        )]);
        context
            .visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: run_id(),
                workflow_type: String::from("fixture"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: namespace_attributes.clone(),
            })
            .await?;
        context
            .visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: WorkflowId::new(uuid::Uuid::from_u128(0xa10a)),
                run_id: RunId::new(uuid::Uuid::from_u128(0xa10b)),
                workflow_type: String::from("aion.schedule_coordinator"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: namespace_attributes,
            })
            .await?;

        let list_request = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: None,
        };
        let response = list(&context.guard, &context.caller, list_request).await?;
        assert_eq!(
            response.summaries.len(),
            1,
            "list must hide engine-internal workflows"
        );
        let summary =
            decode_core_value::<aion_store::visibility::WorkflowSummary>(&response.summaries[0])?;
        assert_eq!(summary.workflow_type, "fixture");

        let count_request = ProtoCountWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: None,
        };
        let response = count(&context.guard, &context.caller, count_request).await?;
        assert_eq!(
            response.count, 1,
            "count must exclude engine-internal workflows"
        );

        // Explicitly enumerating the internal type is still an enumeration
        // surface: it stays hidden.
        let internal_count_request = ProtoCountWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(encode_core_value(
                NAMESPACE,
                None,
                &aion_store::visibility::ListWorkflowsFilter {
                    workflow_type: Some(String::from("aion.schedule_coordinator")),
                    ..aion_store::visibility::ListWorkflowsFilter::default()
                },
            )?),
        };
        let response = count(&context.guard, &context.caller, internal_count_request).await?;
        assert_eq!(
            response.count, 0,
            "counting the internal type directly must report nothing"
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
}
