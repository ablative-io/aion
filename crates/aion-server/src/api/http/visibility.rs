//! Visibility query-string parsing and namespace scoping.

use std::collections::HashMap;

use aion_core::{SearchAttributeValue, WorkflowStatus};
use aion_proto::WireError;
use aion_store::visibility::{ListWorkflowsFilter, SearchAttributePredicate};
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct VisibilityQuery {
    pub(crate) namespace: String,
    workflow_type: Option<String>,
    status: Option<String>,
    started_after: Option<String>,
    started_before: Option<String>,
    closed_after: Option<String>,
    closed_before: Option<String>,
    search_attributes: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    #[serde(flatten)]
    extra: HashMap<String, String>,
}

impl VisibilityQuery {
    pub(crate) fn into_filter(self) -> Result<ListWorkflowsFilter, WireError> {
        let mut search_attributes = self.parse_search_attributes()?;
        search_attributes.extend(parse_attr_equals(self.extra));

        Ok(ListWorkflowsFilter {
            workflow_type: self.workflow_type,
            status: self.status.as_deref().map(parse_status).transpose()?,
            started_after: self
                .started_after
                .as_deref()
                .map(parse_datetime)
                .transpose()?,
            started_before: self
                .started_before
                .as_deref()
                .map(parse_datetime)
                .transpose()?,
            closed_after: self
                .closed_after
                .as_deref()
                .map(parse_datetime)
                .transpose()?,
            closed_before: self
                .closed_before
                .as_deref()
                .map(parse_datetime)
                .transpose()?,
            search_attributes,
            limit: self.limit,
            offset: self.offset,
        })
    }

    fn parse_search_attributes(&self) -> Result<Vec<SearchAttributePredicate>, WireError> {
        self.search_attributes.as_deref().map_or_else(
            || Ok(Vec::new()),
            |value| {
                serde_json::from_str(value).map_err(|_error| {
                    WireError::unknown_query("search_attributes query parameter is malformed")
                })
            },
        )
    }
}

/// Narrow a caller-supplied visibility filter to the authorized namespace.
pub(crate) fn scope_visibility_filter(
    mut filter: ListWorkflowsFilter,
    namespace: &str,
) -> ListWorkflowsFilter {
    filter
        .search_attributes
        .push(SearchAttributePredicate::Equals {
            name: crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
            value: SearchAttributeValue::String(namespace.to_owned()),
        });
    filter
}

fn parse_attr_equals(extra: HashMap<String, String>) -> Vec<SearchAttributePredicate> {
    extra
        .into_iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("attr.")
                .map(|name| SearchAttributePredicate::Equals {
                    name: name.to_owned(),
                    value: SearchAttributeValue::String(value),
                })
        })
        .collect()
}

fn parse_status(value: &str) -> Result<WorkflowStatus, WireError> {
    match value.to_ascii_lowercase().as_str() {
        "running" => Ok(WorkflowStatus::Running),
        "completed" => Ok(WorkflowStatus::Completed),
        "failed" => Ok(WorkflowStatus::Failed),
        "cancelled" | "canceled" => Ok(WorkflowStatus::Cancelled),
        "timed_out" | "timedout" | "timed-out" => Ok(WorkflowStatus::TimedOut),
        "continued_as_new" | "continuedasnew" | "continued-as-new" => {
            Ok(WorkflowStatus::ContinuedAsNew)
        }
        "paused" => Ok(WorkflowStatus::Paused),
        _ => Err(WireError::unknown_query(
            "workflow status query parameter is unknown",
        )),
    }
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>, WireError> {
    DateTime::parse_from_rfc3339(value)
        .map(|datetime| datetime.with_timezone(&Utc))
        .map_err(|_error| WireError::unknown_query("datetime query parameter is malformed"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion::EngineBuilder;
    use aion_core::{SearchAttributeValue, WorkflowId, WorkflowStatus, WorkflowSummary};
    use aion_store::{
        EventStore, InMemoryStore,
        visibility::{VisibilityRecord, VisibilityStore},
    };
    use axum::http::StatusCode;
    use chrono::Utc;
    use tower::ServiceExt;

    use super::super::router::workflow_router;
    use super::super::test_support::{
        get_request, read_json, run_id, runtime_config, server_state, workflow_id,
    };
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::NamespaceMode,
    };

    fn recorded_at(offset_seconds: i64) -> chrono::DateTime<Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
    }

    #[tokio::test]
    async fn get_workflows_and_count_apply_visibility_query_parameters()
    -> Result<(), Box<dyn std::error::Error>> {
        let visibility_store = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = visibility_store.clone();
        let visibility: Arc<dyn VisibilityStore> = visibility_store.clone();
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .visibility_store_arc(Arc::clone(&visibility))
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let router = workflow_router(server_state(resolver, runtime_config()).await?);

        visibility
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: run_id(),
                workflow_type: String::from("checkout"),
                status: WorkflowStatus::Running,
                start_time: recorded_at(1),
                close_time: None,
                failed_step: None,
                failure_reason: None,
                search_attributes: std::collections::HashMap::from([
                    (
                        String::from("customer_id"),
                        SearchAttributeValue::String(String::from("12345")),
                    ),
                    (
                        crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
                        SearchAttributeValue::String(String::from("tenant-a")),
                    ),
                ]),
            })
            .await?;
        visibility
            .record_visibility(VisibilityRecord {
                workflow_id: WorkflowId::new(uuid::Uuid::from_u128(2)),
                run_id: aion_core::RunId::new(uuid::Uuid::from_u128(20)),
                workflow_type: String::from("support"),
                status: WorkflowStatus::Running,
                start_time: recorded_at(2),
                close_time: None,
                failed_step: None,
                failure_reason: None,
                search_attributes: std::collections::HashMap::from([
                    (
                        String::from("customer_id"),
                        SearchAttributeValue::String(String::from("12345")),
                    ),
                    (
                        crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
                        SearchAttributeValue::String(String::from("tenant-a")),
                    ),
                ]),
            })
            .await?;

        let query = concat!(
            "/workflows?namespace=tenant-a",
            "&workflow_type=checkout",
            "&status=running",
            "&started_after=2023-11-14T22%3A13%3A19Z",
            "&started_before=2023-11-14T22%3A13%3A22Z",
            "&limit=10",
            "&offset=0",
            "&attr.customer_id=12345"
        );
        let list_response = router.clone().oneshot(get_request(query)?).await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let summaries: Vec<WorkflowSummary> = read_json(list_response).await?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].workflow_id, workflow_id());

        let count_response = router
            .oneshot(get_request(
                "/workflows/count?namespace=tenant-a&workflow_type=checkout&attr.customer_id=12345",
            )?)
            .await?;
        assert_eq!(count_response.status(), StatusCode::OK);
        let body: serde_json::Value = read_json(count_response).await?;
        assert_eq!(body["count"], 1);
        Ok(())
    }
}
