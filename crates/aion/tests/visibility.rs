//! Visibility integration tests for workflow search attributes.

use std::{collections::HashMap, sync::Arc};

use aion::durability::Recorder;
use aion_core::{
    Payload, SearchAttributeSchema, SearchAttributeType, SearchAttributeValue, WorkflowId,
};
use aion_store::{
    InMemoryStore,
    visibility::{ListWorkflowsFilter, SearchAttributePredicate, VisibilityStore},
};
use chrono::{DateTime, Utc};
use serde_json::json;

#[tokio::test]
async fn workflow_search_attributes_are_queryable_through_visibility_store()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(InMemoryStore::default());
    let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
    let run_id = aion_core::RunId::new(uuid::Uuid::from_u128(10));
    let other_workflow_id = WorkflowId::new(uuid::Uuid::from_u128(2));
    let other_run_id = aion_core::RunId::new(uuid::Uuid::from_u128(20));
    let mut schema = SearchAttributeSchema::new();
    schema.register("customer_id", SearchAttributeType::String)?;

    let mut matching = Recorder::new(workflow_id.clone(), store.clone())
        .with_visibility(run_id.clone(), store.clone());
    matching
        .record_workflow_started(recorded_at(1), String::from("checkout"), payload("input")?)
        .await?;
    matching
        .record_search_attributes_updated(
            recorded_at(2),
            HashMap::from([customer_id_attribute("12345")]),
            &schema,
        )
        .await?;

    let mut other = Recorder::new(other_workflow_id, store.clone())
        .with_visibility(other_run_id, store.clone());
    other
        .record_workflow_started(recorded_at(3), String::from("support"), payload("other")?)
        .await?;
    other
        .record_search_attributes_updated(
            recorded_at(4),
            HashMap::from([customer_id_attribute("12345")]),
            &schema,
        )
        .await?;

    let matching_customer = ListWorkflowsFilter {
        search_attributes: vec![customer_id_equals("12345")],
        ..ListWorkflowsFilter::default()
    };
    let summaries = store.list_workflows(matching_customer.clone()).await?;
    assert!(
        summaries
            .iter()
            .any(|summary| summary.workflow_id == workflow_id)
    );

    let missing_customer = ListWorkflowsFilter {
        search_attributes: vec![customer_id_equals("99999")],
        ..ListWorkflowsFilter::default()
    };
    let summaries = store.list_workflows(missing_customer).await?;
    assert!(
        !summaries
            .iter()
            .any(|summary| summary.workflow_id == workflow_id)
    );

    let narrowed = ListWorkflowsFilter {
        workflow_type: Some(String::from("checkout")),
        search_attributes: vec![customer_id_equals("12345")],
        ..ListWorkflowsFilter::default()
    };
    let summaries = store.list_workflows(narrowed.clone()).await?;
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].workflow_id, workflow_id);
    assert_eq!(summaries[0].run_id, run_id);
    assert_eq!(store.count_workflows(narrowed).await?, 1);
    Ok(())
}

fn customer_id_attribute(value: &str) -> (String, SearchAttributeValue) {
    (
        String::from("customer_id"),
        SearchAttributeValue::String(value.to_owned()),
    )
}

fn customer_id_equals(value: &str) -> SearchAttributePredicate {
    SearchAttributePredicate::Equals {
        name: String::from("customer_id"),
        value: SearchAttributeValue::String(value.to_owned()),
    }
}

fn recorded_at(offset_seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
}

fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "label": label }))
}
