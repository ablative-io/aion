use std::sync::Arc;

use aion_core::{ActivityEvent, ActivityEventKind, ActivityId, MessageRole};
use aion_proto::{WireError, WireErrorCode};
use axum::{Router, http::StatusCode};
use chrono::{DateTime, Utc};
use serde_json::json;
use tower::ServiceExt;

use super::router::workflow_router;
use super::test_support::{
    NAMESPACE, json_request, read_json, runtime_config, server_state, shared_engine, workflow_id,
};
use super::transcripts::TranscriptFetchResponse;
use crate::{
    NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces, config::NamespaceMode,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn transcript_limit_returns_resume_cursor_and_clamps_low() -> TestResult {
    let router = router_with_transcript(5).await?;

    let first = fetch(&router, json!({ "limit": 2 })).await?;
    assert_eq!(store_seqs(&first), vec![0, 1]);
    assert_eq!(first.next_from_seq, Some(2));
    assert_eq!(first.head_seq, 5);

    let mid = fetch(&router, json!({ "from_seq": 2, "limit": 2 })).await?;
    assert_eq!(store_seqs(&mid), vec![2, 3]);
    assert_eq!(mid.next_from_seq, Some(4));
    assert_eq!(mid.head_seq, 5);

    let low = fetch(&router, json!({ "limit": 0 })).await?;
    assert_eq!(store_seqs(&low), vec![0]);
    assert_eq!(low.next_from_seq, Some(1));

    let high = fetch(&router, json!({ "limit": u32::MAX })).await?;
    assert_eq!(store_seqs(&high), vec![0, 1, 2, 3, 4]);
    assert_eq!(high.next_from_seq, None);
    Ok(())
}

#[tokio::test]
async fn transcript_last_opens_the_retained_tail() -> TestResult {
    let router = router_with_transcript(5).await?;

    let tail = fetch(&router, json!({ "last": 2 })).await?;
    assert_eq!(store_seqs(&tail), vec![3, 4]);
    assert_eq!(tail.next_from_seq, None);
    assert_eq!(tail.head_seq, 5);

    let empty_tail = fetch(&router, json!({ "last": 0 })).await?;
    assert!(empty_tail.events.is_empty());
    assert_eq!(empty_tail.next_from_seq, None);
    assert_eq!(empty_tail.head_seq, 5);
    Ok(())
}

#[tokio::test]
async fn transcript_request_without_new_fields_preserves_full_replay_bytes() -> TestResult {
    let router = router_with_transcript(5).await?;
    let body = fetch(&router, json!({})).await?;
    let expected = (0..5)
        .map(|seq| {
            let mut event = activity_event(seq);
            event.store_seq = Some(seq);
            event
        })
        .collect::<Vec<_>>();

    assert_eq!(
        serde_json::to_vec(&body.events)?,
        serde_json::to_vec(&expected)?
    );
    assert_eq!(body.next_from_seq, None);
    assert_eq!(body.head_seq, 5);
    Ok(())
}

#[tokio::test]
async fn transcript_rejects_last_with_from_seq_as_typed_input_error() -> TestResult {
    let router = router_with_transcript(1).await?;
    let request = transcript_request(json!({ "from_seq": 0, "last": 1 }))?;
    let response = router
        .oneshot(json_request("/workflows/transcript", &request)?)
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: WireError = read_json(response).await?;
    assert_eq!(error.code, WireErrorCode::InvalidInput);
    assert!(error.message.contains("mutually exclusive"));
    Ok(())
}

async fn fetch(
    router: &Router,
    fields: serde_json::Value,
) -> Result<TranscriptFetchResponse, Box<dyn std::error::Error>> {
    let request = transcript_request(fields)?;
    let response = router
        .clone()
        .oneshot(json_request("/workflows/transcript", &request)?)
        .await?;
    if response.status() != StatusCode::OK {
        return Err(format!("transcript request failed with {}", response.status()).into());
    }
    read_json(response).await
}

fn transcript_request(
    fields: serde_json::Value,
) -> Result<serde_json::Map<String, serde_json::Value>, Box<dyn std::error::Error>> {
    let mut request = match fields {
        serde_json::Value::Object(request) => request,
        _other => return Err("transcript request fields must be an object".into()),
    };
    request.insert("namespace".to_owned(), json!(NAMESPACE));
    request.insert("workflow_id".to_owned(), json!(workflow_id()));
    request.insert("activity_id".to_owned(), json!(3));
    request.insert("attempt".to_owned(), json!(1));
    Ok(request)
}

fn store_seqs(response: &TranscriptFetchResponse) -> Vec<u64> {
    response
        .events
        .iter()
        .filter_map(|event| event.store_seq)
        .collect()
}

async fn router_with_transcript(count: u64) -> Result<Router, Box<dyn std::error::Error>> {
    let (engine, _store, _visibility) = shared_engine().await?;
    let ownership = StaticWorkflowNamespaces::default();
    ownership.record(workflow_id(), NAMESPACE)?;
    let resolver = NamespaceResolver::from_parts(
        NamespaceMode::SharedEngine,
        Some(engine),
        Arc::new(ownership),
        Arc::new(StaticScheduleNamespaces::default()),
    );
    let state = server_state(resolver, runtime_config()).await?;
    for seq in 0..count {
        let assigned = state
            .transcript_publisher()
            .publish(&activity_event(seq))
            .await?;
        if assigned != Some(seq) {
            return Err(format!("expected transcript store_seq {seq}, got {assigned:?}").into());
        }
    }
    Ok(workflow_router(state))
}

fn activity_event(worker_seq: u64) -> ActivityEvent {
    ActivityEvent {
        workflow_id: workflow_id(),
        activity_id: ActivityId::from_sequence_position(3),
        attempt: 1,
        agent_id: uuid::Uuid::from_u128(42),
        agent_role: "operator".to_owned(),
        emitted_at: DateTime::<Utc>::UNIX_EPOCH,
        worker_seq,
        store_seq: None,
        ephemeral: false,
        kind: ActivityEventKind::Message {
            role: MessageRole::Assistant,
            text: format!("event-{worker_seq}"),
        },
    }
}
