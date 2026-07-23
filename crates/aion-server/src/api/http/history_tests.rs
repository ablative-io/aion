use std::sync::Arc;

use aion_core::{ContentType, Event, EventEnvelope, PackageVersion, Payload, RunId};
use aion_proto::{WireError, WireErrorCode};
use aion_store::WriteToken;
use axum::{Router, http::StatusCode};
use chrono::Utc;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::router::workflow_router;
use super::test_support::{
    NAMESPACE, json_request, read_json, runtime_config, server_state, shared_engine, workflow_id,
};
use crate::{
    NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces, config::NamespaceMode,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn history_paginates_default_mid_head_and_beyond_head() -> TestResult {
    let router = router_with_history(cancelled_events(5)).await?;

    let first = history(&router, json!({ "limit": 2 })).await?;
    assert_eq!(event_seqs(&first)?, vec![1, 2]);
    assert_eq!(first["next_from_seq"], 3);
    assert_eq!(first["head_seq"], 5);

    let mid = history(&router, json!({ "from_seq": 3, "limit": 2 })).await?;
    assert_eq!(event_seqs(&mid)?, vec![3, 4]);
    assert_eq!(mid["next_from_seq"], 5);
    assert_eq!(mid["head_seq"], 5);

    let at_head = history(&router, json!({ "from_seq": 5 })).await?;
    assert_eq!(event_seqs(&at_head)?, vec![5]);
    assert!(at_head["next_from_seq"].is_null());
    assert_eq!(at_head["head_seq"], 5);

    let beyond = history(&router, json!({ "from_seq": 6 })).await?;
    assert!(event_seqs(&beyond)?.is_empty());
    assert!(beyond["next_from_seq"].is_null());
    assert_eq!(beyond["head_seq"], 5);
    Ok(())
}

#[tokio::test]
async fn history_clamps_low_and_high_limits_and_handles_empty_history() -> TestResult {
    let router = router_with_history(cancelled_events(2_001)).await?;
    let default = history(&router, json!({})).await?;
    assert_eq!(event_seqs(&default)?.len(), 500);
    assert_eq!(default["next_from_seq"], 501);

    let low = history(&router, json!({ "limit": 0 })).await?;
    assert_eq!(event_seqs(&low)?, vec![1]);
    assert_eq!(low["next_from_seq"], 2);

    let high = history(&router, json!({ "limit": u32::MAX })).await?;
    assert_eq!(event_seqs(&high)?.len(), 2_000);
    assert_eq!(high["next_from_seq"], 2_001);
    assert_eq!(high["head_seq"], 2_001);

    let empty_router = router_with_history(Vec::new()).await?;
    let empty = history(&empty_router, json!({})).await?;
    assert!(event_seqs(&empty)?.is_empty());
    assert!(empty["next_from_seq"].is_null());
    assert_eq!(empty["head_seq"], 0);
    Ok(())
}

#[tokio::test]
async fn history_elides_only_payload_bytes_strictly_above_the_boundary() -> TestResult {
    let router = router_with_history(vec![payload_event(1, 4), payload_event(2, 5)]).await?;
    let body = history(&router, json!({ "payload_limit_bytes": 4 })).await?;

    assert_eq!(
        body["events"][0]["data"]["input"]["bytes"],
        json!([7, 7, 7, 7])
    );
    assert_eq!(
        body["events"][1]["data"]["input"]["bytes"],
        json!({ "__elided": true, "size_bytes": 5 })
    );
    assert_eq!(body["events"][1]["type"], "WorkflowStarted");
    assert_eq!(body["events"][1]["data"]["envelope"]["seq"], 2);
    assert!(body["events"][1]["data"]["envelope"]["recorded_at"].is_string());
    assert_eq!(
        body["events"][1]["data"]["envelope"]["workflow_id"],
        workflow_id().to_string()
    );
    assert_eq!(body["events"][1]["data"]["workflow_type"], "fixture");

    let full = history(&router, json!({ "payload_limit_bytes": 0 })).await?;
    assert_eq!(
        full["events"][1]["data"]["input"]["bytes"],
        json!([7, 7, 7, 7, 7])
    );
    assert!(full["events"][1]["data"]["input"]["bytes"].is_array());
    Ok(())
}

#[tokio::test]
async fn event_fetch_returns_full_payload_and_typed_not_found() -> TestResult {
    let router = router_with_history(vec![payload_event(1, 5)]).await?;
    let request = json!({
        "namespace": NAMESPACE,
        "workflow_id": workflow_id().to_string(),
        "seq": 1,
    });
    let response = router
        .clone()
        .oneshot(json_request("/workflows/event", &request)?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = read_json(response).await?;
    assert_eq!(
        body["event"]["data"]["input"]["bytes"],
        json!([7, 7, 7, 7, 7])
    );
    assert!(body["event"]["data"]["input"]["bytes"].is_array());

    let missing = json!({
        "namespace": NAMESPACE,
        "workflow_id": workflow_id().to_string(),
        "seq": 2,
    });
    let response = router
        .oneshot(json_request("/workflows/event", &missing)?)
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error: WireError = read_json(response).await?;
    assert_eq!(error.code, WireErrorCode::NotFound);
    assert!(error.message.contains("seq 2"));
    Ok(())
}

async fn history(router: &Router, fields: Value) -> Result<Value, Box<dyn std::error::Error>> {
    let mut request = fields
        .as_object()
        .cloned()
        .ok_or("history request fields must be an object")?;
    request.insert("namespace".to_owned(), json!(NAMESPACE));
    request.insert("workflow_id".to_owned(), json!(workflow_id().to_string()));
    let response = router
        .clone()
        .oneshot(json_request("/workflows/history", &request)?)
        .await?;
    if response.status() != StatusCode::OK {
        return Err(format!("history request failed with {}", response.status()).into());
    }
    read_json(response).await
}

fn event_seqs(body: &Value) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
    body["events"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("events missing"))?
        .iter()
        .map(|event| {
            event["data"]["envelope"]["seq"]
                .as_u64()
                .ok_or_else(|| std::io::Error::other("event seq missing").into())
        })
        .collect()
}

async fn router_with_history(events: Vec<Event>) -> Result<Router, Box<dyn std::error::Error>> {
    let (engine, store, _visibility) = shared_engine().await?;
    if !events.is_empty() {
        store
            .append(WriteToken::recorder(), &workflow_id(), &events, 0)
            .await?;
    }
    let ownership = StaticWorkflowNamespaces::default();
    ownership.record(workflow_id(), NAMESPACE)?;
    let resolver = NamespaceResolver::from_parts(
        NamespaceMode::SharedEngine,
        Some(engine),
        Arc::new(ownership),
        Arc::new(StaticScheduleNamespaces::default()),
    );
    let state = server_state(resolver, runtime_config()).await?;
    Ok(workflow_router(state))
}

fn cancelled_events(count: u64) -> Vec<Event> {
    (1..=count)
        .map(|seq| Event::WorkflowCancelled {
            envelope: envelope(seq),
            reason: format!("cancelled-{seq}"),
        })
        .collect()
}

fn payload_event(seq: u64, size: usize) -> Event {
    Event::WorkflowStarted {
        envelope: envelope(seq),
        workflow_type: "fixture".to_owned(),
        input: Payload::new(ContentType::Json, vec![7; size]),
        run_id: RunId::new(uuid::Uuid::from_u128(u128::from(seq))),
        parent_run_id: None,
        package_version: PackageVersion::new("a".repeat(64)),
    }
}

fn envelope(seq: u64) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: Utc::now(),
        workflow_id: workflow_id(),
    }
}
