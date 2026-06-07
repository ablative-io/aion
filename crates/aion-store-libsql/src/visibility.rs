//! Workflow visibility projection storage backed by libSQL.

use std::collections::HashMap;

use aion_core::{RunId, SearchAttributeValue, WorkflowId, WorkflowStatus};
use aion_store::{
    ListWorkflowsFilter, SearchAttributePredicate, StoreError, VisibilityRecord, VisibilityStore,
    VisibilityWorkflowSummary,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use libsql::{Value, params_from_iter};
use uuid::Uuid;

use crate::store::LibSqlStore;

const UPSERT_VISIBILITY_SQL: &str = "
INSERT OR REPLACE INTO visibility (
    workflow_id,
    run_id,
    workflow_type,
    status,
    start_time,
    close_time,
    search_attributes
)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

/// Upsert a complete workflow visibility projection row.
///
/// # Errors
///
/// Returns `StoreError::Serialization` when record fields cannot be encoded and
/// `StoreError::Backend` when libSQL rejects the upsert.
pub(crate) async fn record_visibility(
    conn: &libsql::Connection,
    record: VisibilityRecord,
) -> Result<(), StoreError> {
    let workflow_id = record.workflow_id.to_string();
    let run_id = record.run_id.to_string();
    let status = encode_status(record.status)?;
    let start_time = encode_timestamp(record.start_time);
    let close_time = record.close_time.map(encode_timestamp);
    let search_attributes = serde_json::to_string(&record.search_attributes)
        .map_err(|error| crate::error::serde_json_error(&error))?;

    conn.execute(
        UPSERT_VISIBILITY_SQL,
        (
            workflow_id,
            run_id,
            record.workflow_type,
            status,
            start_time,
            close_time,
            search_attributes,
        ),
    )
    .await
    .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(())
}

/// List workflow visibility summaries matching `filter`.
///
/// # Errors
///
/// Returns backend errors from libSQL or serialization errors when persisted rows cannot be decoded.
pub(crate) async fn list_workflows(
    conn: &libsql::Connection,
    filter: ListWorkflowsFilter,
) -> Result<Vec<VisibilityWorkflowSummary>, StoreError> {
    let plan = QueryPlan::list(&filter)?;
    let mut rows = conn
        .query(&plan.sql, params_from_iter(plan.params))
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut summaries = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        summaries.push(decode_summary(&row)?);
    }

    Ok(summaries)
}

/// Count workflow visibility summaries matching `filter`, ignoring pagination fields.
///
/// # Errors
///
/// Returns backend errors from libSQL or serialization errors from filter encoding.
pub(crate) async fn count_workflows(
    conn: &libsql::Connection,
    filter: ListWorkflowsFilter,
) -> Result<u64, StoreError> {
    let plan = QueryPlan::count(&filter)?;
    let mut rows = conn
        .query(&plan.sql, params_from_iter(plan.params))
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
        .ok_or_else(|| {
            StoreError::Backend(String::from("visibility count query returned no row"))
        })?;
    let count: i64 = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;

    u64::try_from(count).map_err(|error| StoreError::Backend(error.to_string()))
}

#[async_trait]
impl VisibilityStore for LibSqlStore {
    async fn record_visibility(&self, record: VisibilityRecord) -> Result<(), StoreError> {
        record_visibility(self.connection(), record).await
    }

    async fn list_workflows(
        &self,
        filter: ListWorkflowsFilter,
    ) -> Result<Vec<VisibilityWorkflowSummary>, StoreError> {
        list_workflows(self.connection(), filter).await
    }

    async fn count_workflows(&self, filter: ListWorkflowsFilter) -> Result<u64, StoreError> {
        count_workflows(self.connection(), filter).await
    }
}

struct QueryPlan {
    sql: String,
    params: Vec<Value>,
}

impl QueryPlan {
    fn list(filter: &ListWorkflowsFilter) -> Result<Self, StoreError> {
        let mut plan = Self::filtered(
            "SELECT workflow_id, run_id, workflow_type, status, start_time, close_time, search_attributes FROM visibility",
            filter,
        )?;
        plan.sql
            .push_str(" ORDER BY start_time DESC, workflow_id ASC");
        if let Some(limit) = filter.limit {
            plan.push_param(Value::Integer(i64::from(limit)));
            plan.sql.push_str(&format!(" LIMIT ?{}", plan.params.len()));
        }
        if let Some(offset) = filter.offset {
            plan.push_param(Value::Integer(i64::from(offset)));
            plan.sql
                .push_str(&format!(" OFFSET ?{}", plan.params.len()));
        }
        Ok(plan)
    }

    fn count(filter: &ListWorkflowsFilter) -> Result<Self, StoreError> {
        Self::filtered("SELECT COUNT(*) FROM visibility", filter)
    }

    fn filtered(base_sql: &str, filter: &ListWorkflowsFilter) -> Result<Self, StoreError> {
        let mut builder = FilterBuilder::default();
        builder.add_filter(filter)?;
        let (where_sql, params) = builder.finish();
        let sql = if where_sql.is_empty() {
            String::from(base_sql)
        } else {
            format!("{base_sql} WHERE {where_sql}")
        };

        Ok(Self { sql, params })
    }

    fn push_param(&mut self, value: Value) {
        self.params.push(value);
    }
}

#[derive(Default)]
struct FilterBuilder {
    clauses: Vec<String>,
    params: Vec<Value>,
}

impl FilterBuilder {
    fn add_filter(&mut self, filter: &ListWorkflowsFilter) -> Result<(), StoreError> {
        if let Some(workflow_type) = &filter.workflow_type {
            self.push_clause("workflow_type =", Value::Text(workflow_type.clone()));
        }
        if let Some(status) = filter.status {
            self.push_clause("status =", Value::Text(encode_status(status)?));
        }
        if let Some(started_after) = filter.started_after {
            self.push_clause(
                "start_time >=",
                Value::Text(encode_timestamp(started_after)),
            );
        }
        if let Some(started_before) = filter.started_before {
            self.push_clause(
                "start_time <=",
                Value::Text(encode_timestamp(started_before)),
            );
        }
        if let Some(closed_after) = filter.closed_after {
            self.push_clause("close_time >=", Value::Text(encode_timestamp(closed_after)));
        }
        if let Some(closed_before) = filter.closed_before {
            self.push_clause(
                "close_time <=",
                Value::Text(encode_timestamp(closed_before)),
            );
        }
        for predicate in &filter.search_attributes {
            self.add_search_attribute_predicate(predicate)?;
        }

        Ok(())
    }

    fn push_clause(&mut self, lhs_and_operator: &str, value: Value) {
        self.params.push(value);
        self.clauses
            .push(format!("{lhs_and_operator} ?{}", self.params.len()));
    }

    fn add_search_attribute_predicate(
        &mut self,
        predicate: &SearchAttributePredicate,
    ) -> Result<(), StoreError> {
        match predicate {
            SearchAttributePredicate::Equals { name, value } => self.add_equals(name, value),
            SearchAttributePredicate::GreaterThan { name, value } => {
                self.add_ordered_comparison(name, value, ">")
            }
            SearchAttributePredicate::LessThan { name, value } => {
                self.add_ordered_comparison(name, value, "<")
            }
            SearchAttributePredicate::Contains { name, keyword } => {
                self.add_contains(name, keyword)
            }
        }
    }

    fn add_equals(&mut self, name: &str, value: &SearchAttributeValue) -> Result<(), StoreError> {
        let type_path = search_attribute_path(name, "type")?;
        let data_path = search_attribute_path(name, "data")?;
        let type_param = self.push(Value::Text(type_name(value)));
        let data_param = self.push(search_attribute_data_value(value)?);
        let type_path_param = self.push(Value::Text(type_path));
        let data_path_param = self.push(Value::Text(data_path));
        self.clauses.push(format!(
            "json_extract(search_attributes, ?{type_path_param}) = ?{type_param} AND json_extract(search_attributes, ?{data_path_param}) = ?{data_param}"
        ));
        Ok(())
    }

    fn add_ordered_comparison(
        &mut self,
        name: &str,
        value: &SearchAttributeValue,
        operator: &str,
    ) -> Result<(), StoreError> {
        if !is_ordered_value(value) {
            self.clauses.push(String::from("0 = 1"));
            return Ok(());
        }

        let type_path = search_attribute_path(name, "type")?;
        let data_path = search_attribute_path(name, "data")?;
        let type_param = self.push(Value::Text(type_name(value)));
        let data_param = self.push(search_attribute_data_value(value)?);
        let type_path_param = self.push(Value::Text(type_path));
        let data_path_param = self.push(Value::Text(data_path));
        self.clauses.push(format!(
            "json_extract(search_attributes, ?{type_path_param}) = ?{type_param} AND json_extract(search_attributes, ?{data_path_param}) {operator} ?{data_param}"
        ));
        Ok(())
    }

    fn add_contains(&mut self, name: &str, keyword: &str) -> Result<(), StoreError> {
        let type_path = search_attribute_path(name, "type")?;
        let data_path = search_attribute_path(name, "data")?;
        let type_param = self.push(Value::Text(String::from("KeywordList")));
        let keyword_param = self.push(Value::Text(String::from(keyword)));
        let type_path_param = self.push(Value::Text(type_path));
        let data_path_param = self.push(Value::Text(data_path));
        self.clauses.push(format!(
            "json_extract(search_attributes, ?{type_path_param}) = ?{type_param} AND EXISTS (SELECT 1 FROM json_each(json_extract(search_attributes, ?{data_path_param})) WHERE value = ?{keyword_param})"
        ));
        Ok(())
    }

    fn push(&mut self, value: Value) -> usize {
        self.params.push(value);
        self.params.len()
    }

    fn finish(self) -> (String, Vec<Value>) {
        (self.clauses.join(" AND "), self.params)
    }
}

fn decode_summary(row: &libsql::Row) -> Result<VisibilityWorkflowSummary, StoreError> {
    let workflow_id: String = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let run_id: String = row
        .get(1)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let workflow_type: String = row
        .get(2)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let status: String = row
        .get(3)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let start_time: String = row
        .get(4)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let close_time: Option<String> = row
        .get(5)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let search_attributes: String = row
        .get(6)
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(VisibilityWorkflowSummary {
        workflow_id: decode_workflow_id(&workflow_id)?,
        run_id: decode_run_id(&run_id)?,
        workflow_type,
        status: decode_status(&status)?,
        start_time: decode_timestamp(&start_time)?,
        close_time: close_time.as_deref().map(decode_timestamp).transpose()?,
        search_attributes: serde_json::from_str::<HashMap<String, SearchAttributeValue>>(
            &search_attributes,
        )
        .map_err(|error| crate::error::serde_json_error(&error))?,
    })
}

fn encode_status(status: WorkflowStatus) -> Result<String, StoreError> {
    serde_json::to_string(&status).map_err(|error| crate::error::serde_json_error(&error))
}

fn decode_status(value: &str) -> Result<WorkflowStatus, StoreError> {
    serde_json::from_str(value).map_err(|error| crate::error::serde_json_error(&error))
}

fn encode_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date_time| date_time.with_timezone(&Utc))
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

fn decode_workflow_id(value: &str) -> Result<WorkflowId, StoreError> {
    Uuid::parse_str(value)
        .map(WorkflowId::new)
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

fn decode_run_id(value: &str) -> Result<RunId, StoreError> {
    Uuid::parse_str(value)
        .map(RunId::new)
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

fn search_attribute_path(name: &str, field: &str) -> Result<String, StoreError> {
    let quoted_name =
        serde_json::to_string(name).map_err(|error| crate::error::serde_json_error(&error))?;
    Ok(format!("$.{quoted_name}.{field}"))
}

fn type_name(value: &SearchAttributeValue) -> String {
    match value {
        SearchAttributeValue::String(_) => String::from("String"),
        SearchAttributeValue::Int(_) => String::from("Int"),
        SearchAttributeValue::Float(_) => String::from("Float"),
        SearchAttributeValue::Bool(_) => String::from("Bool"),
        SearchAttributeValue::Datetime(_) => String::from("Datetime"),
        SearchAttributeValue::KeywordList(_) => String::from("KeywordList"),
    }
}

fn search_attribute_data_value(value: &SearchAttributeValue) -> Result<Value, StoreError> {
    match value {
        SearchAttributeValue::String(value) => Ok(Value::Text(value.clone())),
        SearchAttributeValue::Int(value) => Ok(Value::Integer(*value)),
        SearchAttributeValue::Float(value) => Ok(Value::Real(*value)),
        SearchAttributeValue::Bool(value) => Ok(Value::Integer(i64::from(*value))),
        SearchAttributeValue::Datetime(value) => {
            Ok(Value::Text(serde_search_attribute_data(value)?))
        }
        SearchAttributeValue::KeywordList(values) => serde_json::to_string(values)
            .map(Value::Text)
            .map_err(|error| crate::error::serde_json_error(&error)),
    }
}

fn serde_search_attribute_data(value: &DateTime<Utc>) -> Result<String, StoreError> {
    let json = serde_json::to_value(SearchAttributeValue::Datetime(value.to_owned()))
        .map_err(|error| crate::error::serde_json_error(&error))?;
    json.get("data")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
        .ok_or_else(|| {
            StoreError::Serialization(String::from(
                "datetime search attribute encoded without string data",
            ))
        })
}

const fn is_ordered_value(value: &SearchAttributeValue) -> bool {
    matches!(
        value,
        SearchAttributeValue::Int(_)
            | SearchAttributeValue::Float(_)
            | SearchAttributeValue::Datetime(_)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_core::{RunId, SearchAttributeValue, WorkflowId, WorkflowStatus};
    use aion_store::{
        ListWorkflowsFilter, SearchAttributePredicate, StoreError, VisibilityRecord,
        VisibilityStore, VisibilityWorkflowSummary,
    };
    use chrono::{DateTime, TimeZone, Utc};

    use super::{
        count_workflows, decode_timestamp, encode_timestamp, list_workflows, record_visibility,
    };
    use crate::config::{LibSqlConfig, LibSqlMode};

    #[tokio::test]
    async fn record_visibility_inserts_and_updates_queryable_row() -> Result<(), StoreError> {
        let conn = open_test_connection("upsert").await?;
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let mut record = visibility_record(
            workflow_id.clone(),
            run_id.clone(),
            "orders",
            WorkflowStatus::Running,
            instant(2026, 6, 1, 9, 0, 0)?,
            None,
        );
        record_visibility(&conn, record.clone()).await?;

        record.status = WorkflowStatus::Completed;
        record.close_time = Some(instant(2026, 6, 1, 10, 0, 0)?);
        record.search_attributes.insert(
            String::from("customer"),
            SearchAttributeValue::String(String::from("cust-2")),
        );
        record_visibility(&conn, record.clone()).await?;

        let summaries = list_workflows(&conn, ListWorkflowsFilter::default()).await?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0], record.into());

        Ok(())
    }

    #[tokio::test]
    async fn list_filters_standard_fields_and_time_ranges() -> Result<(), StoreError> {
        let conn = open_test_connection("standard-filters").await?;
        let first = seed_record(
            &conn,
            "orders",
            WorkflowStatus::Completed,
            instant(2026, 6, 1, 9, 0, 0)?,
            Some(instant(2026, 6, 1, 10, 0, 0)?),
            "cust-1",
            2,
        )
        .await?;
        let second = seed_record(
            &conn,
            "billing",
            WorkflowStatus::Running,
            instant(2026, 6, 2, 9, 0, 0)?,
            None,
            "cust-2",
            5,
        )
        .await?;

        assert_eq!(
            ids(list_workflows(
                &conn,
                ListWorkflowsFilter {
                    workflow_type: Some(String::from("orders")),
                    ..ListWorkflowsFilter::default()
                },
            )
            .await?),
            vec![first.workflow_id.clone()]
        );
        assert_eq!(
            ids(list_workflows(
                &conn,
                ListWorkflowsFilter {
                    status: Some(WorkflowStatus::Running),
                    ..ListWorkflowsFilter::default()
                },
            )
            .await?),
            vec![second.workflow_id.clone()]
        );
        assert_eq!(
            ids(list_workflows(
                &conn,
                ListWorkflowsFilter {
                    started_after: Some(instant(2026, 6, 2, 0, 0, 0)?),
                    started_before: Some(instant(2026, 6, 2, 23, 59, 59)?),
                    ..ListWorkflowsFilter::default()
                },
            )
            .await?),
            vec![second.workflow_id.clone()]
        );
        assert_eq!(
            ids(list_workflows(
                &conn,
                ListWorkflowsFilter {
                    closed_after: Some(instant(2026, 6, 1, 9, 30, 0)?),
                    closed_before: Some(instant(2026, 6, 1, 10, 30, 0)?),
                    ..ListWorkflowsFilter::default()
                },
            )
            .await?),
            vec![first.workflow_id.clone()]
        );

        Ok(())
    }

    #[tokio::test]
    async fn list_filters_custom_search_attributes() -> Result<(), StoreError> {
        let conn = open_test_connection("custom-filters").await?;
        let first = seed_record(
            &conn,
            "orders",
            WorkflowStatus::Completed,
            instant(2026, 6, 1, 9, 0, 0)?,
            Some(instant(2026, 6, 1, 10, 0, 0)?),
            "cust-1",
            2,
        )
        .await?;
        let second = seed_record(
            &conn,
            "orders",
            WorkflowStatus::Completed,
            instant(2026, 6, 2, 9, 0, 0)?,
            Some(instant(2026, 6, 2, 10, 0, 0)?),
            "cust-2",
            5,
        )
        .await?;

        assert_eq!(
            ids(list_workflows(
                &conn,
                custom_filter(SearchAttributePredicate::Equals {
                    name: String::from("customer"),
                    value: SearchAttributeValue::String(String::from("cust-1")),
                })
            )
            .await?),
            vec![first.workflow_id.clone()]
        );
        assert_eq!(
            ids(list_workflows(
                &conn,
                custom_filter(SearchAttributePredicate::GreaterThan {
                    name: String::from("attempts"),
                    value: SearchAttributeValue::Int(3),
                })
            )
            .await?),
            vec![second.workflow_id.clone()]
        );
        assert_eq!(
            ids(list_workflows(
                &conn,
                custom_filter(SearchAttributePredicate::LessThan {
                    name: String::from("attempts"),
                    value: SearchAttributeValue::Int(3),
                })
            )
            .await?),
            vec![first.workflow_id.clone()]
        );
        assert_eq!(
            ids(list_workflows(
                &conn,
                custom_filter(SearchAttributePredicate::Contains {
                    name: String::from("tags"),
                    keyword: String::from("west"),
                })
            )
            .await?),
            vec![second.workflow_id.clone(), first.workflow_id.clone()]
        );

        Ok(())
    }

    #[tokio::test]
    async fn list_empty_filter_orders_by_start_time_desc_and_paginates() -> Result<(), StoreError> {
        let conn = open_test_connection("pagination").await?;
        let oldest = seed_record(
            &conn,
            "orders",
            WorkflowStatus::Running,
            instant(2026, 6, 1, 9, 0, 0)?,
            None,
            "cust-1",
            1,
        )
        .await?;
        let middle = seed_record(
            &conn,
            "orders",
            WorkflowStatus::Running,
            instant(2026, 6, 2, 9, 0, 0)?,
            None,
            "cust-2",
            2,
        )
        .await?;
        let newest = seed_record(
            &conn,
            "orders",
            WorkflowStatus::Running,
            instant(2026, 6, 3, 9, 0, 0)?,
            None,
            "cust-3",
            3,
        )
        .await?;

        assert_eq!(
            ids(list_workflows(&conn, ListWorkflowsFilter::default()).await?),
            vec![
                newest.workflow_id,
                middle.workflow_id.clone(),
                oldest.workflow_id
            ]
        );
        assert_eq!(
            ids(list_workflows(
                &conn,
                ListWorkflowsFilter {
                    limit: Some(1),
                    offset: Some(1),
                    ..ListWorkflowsFilter::default()
                },
            )
            .await?),
            vec![middle.workflow_id]
        );

        Ok(())
    }

    #[tokio::test]
    async fn count_workflows_counts_total_and_filtered_matches() -> Result<(), StoreError> {
        let conn = open_test_connection("count").await?;
        seed_record(
            &conn,
            "orders",
            WorkflowStatus::Completed,
            instant(2026, 6, 1, 9, 0, 0)?,
            Some(instant(2026, 6, 1, 10, 0, 0)?),
            "cust-1",
            2,
        )
        .await?;
        seed_record(
            &conn,
            "orders",
            WorkflowStatus::Running,
            instant(2026, 6, 2, 9, 0, 0)?,
            None,
            "cust-2",
            5,
        )
        .await?;

        assert_eq!(
            count_workflows(&conn, ListWorkflowsFilter::default()).await?,
            2
        );
        let filter = ListWorkflowsFilter {
            status: Some(WorkflowStatus::Completed),
            ..ListWorkflowsFilter::default()
        };
        assert_eq!(count_workflows(&conn, filter.clone()).await?, 1);
        assert_eq!(
            count_workflows(&conn, filter.clone()).await? as usize,
            list_workflows(&conn, filter).await?.len()
        );

        Ok(())
    }

    #[tokio::test]
    async fn libsql_store_implements_visibility_store_trait() -> Result<(), StoreError> {
        let store = crate::store::LibSqlStore::open(unique_temp_path("trait")).await?;
        let store: std::sync::Arc<dyn VisibilityStore> = std::sync::Arc::new(store);

        assert_eq!(std::sync::Arc::strong_count(&store), 1);
        Ok(())
    }

    #[test]
    fn timestamp_encoding_round_trips_losslessly() -> Result<(), StoreError> {
        let timestamp = instant(2026, 6, 3, 2, 30, 0)?;

        assert_eq!(decode_timestamp(&encode_timestamp(timestamp))?, timestamp);
        Ok(())
    }

    async fn seed_record(
        conn: &libsql::Connection,
        workflow_type: &str,
        status: WorkflowStatus,
        start_time: DateTime<Utc>,
        close_time: Option<DateTime<Utc>>,
        customer: &str,
        attempts: i64,
    ) -> Result<VisibilityRecord, StoreError> {
        let record = visibility_record(
            WorkflowId::new_v4(),
            RunId::new_v4(),
            workflow_type,
            status,
            start_time,
            close_time,
        )
        .with_customer(customer, attempts);
        record_visibility(conn, record.clone()).await?;
        Ok(record)
    }

    trait RecordBuilder {
        fn with_customer(self, customer: &str, attempts: i64) -> Self;
    }

    impl RecordBuilder for VisibilityRecord {
        fn with_customer(mut self, customer: &str, attempts: i64) -> Self {
            self.search_attributes.insert(
                String::from("customer"),
                SearchAttributeValue::String(String::from(customer)),
            );
            self.search_attributes.insert(
                String::from("attempts"),
                SearchAttributeValue::Int(attempts),
            );
            self.search_attributes.insert(
                String::from("tags"),
                SearchAttributeValue::KeywordList(vec![String::from("vip"), String::from("west")]),
            );
            self
        }
    }

    fn visibility_record(
        workflow_id: WorkflowId,
        run_id: RunId,
        workflow_type: &str,
        status: WorkflowStatus,
        start_time: DateTime<Utc>,
        close_time: Option<DateTime<Utc>>,
    ) -> VisibilityRecord {
        VisibilityRecord {
            workflow_id,
            run_id,
            workflow_type: String::from(workflow_type),
            status,
            start_time,
            close_time,
            search_attributes: HashMap::new(),
        }
    }

    fn custom_filter(predicate: SearchAttributePredicate) -> ListWorkflowsFilter {
        ListWorkflowsFilter {
            search_attributes: vec![predicate],
            ..ListWorkflowsFilter::default()
        }
    }

    fn ids(summaries: Vec<VisibilityWorkflowSummary>) -> Vec<WorkflowId> {
        summaries
            .into_iter()
            .map(|summary| summary.workflow_id)
            .collect()
    }

    async fn open_test_connection(name: &str) -> Result<libsql::Connection, StoreError> {
        let config = LibSqlConfig {
            mode: LibSqlMode::Embedded {
                path: unique_temp_path(name),
            },
            journal_mode: None,
            synchronous: None,
            sync_interval_seconds: None,
        };
        let conn = crate::connection::open_connection(&config)
            .await?
            .connection;
        crate::schema::ensure_schema(&conn).await?;
        Ok(conn)
    }

    fn instant(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Result<DateTime<Utc>, StoreError> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .ok_or_else(|| StoreError::Serialization(String::from("invalid test instant")))
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-store-libsql-visibility-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }
}
