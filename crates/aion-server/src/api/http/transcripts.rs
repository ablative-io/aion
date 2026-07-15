//! Durable transcript read API (lane #229): the REST twin of the events pair.
//!
//! `POST /workflows/transcript` fetches the retained transcript of one
//! `(workflow, activity, attempt)` stream from the durable `O` keyspace, and
//! `POST /workflows/transcripts` enumerates a workflow's retained streams.
//! Together they mirror the workflow-history pair: `/workflows/describe` is the
//! socket-free history read, the `/events/stream` subscription is the live
//! tail — and for transcripts this pair is the socket-free read while the
//! `/events/stream` transcript subscription remains the live-tail attach. A
//! client that wants both does a REST fetch first, then attaches the WS with
//! `after_seq` = the last fetched `store_seq` (the publisher's subscribe path
//! dedups the splice seam on `store_seq`).
//!
//! Both endpoints are namespace-scoped through the SAME per-workflow gate the
//! transcript WS subscription uses ([`gate_transcript_workflow`]), so a caller
//! probing a foreign or nonexistent workflow receives the guard's anti-leak
//! `not_found`, never a transcript. An authorized workflow with nothing
//! retained answers `200` with an empty list — the honest answer for an old
//! run recorded before retention existed, never an error.

use aion_core::{ActivityEvent, ActivityId, WorkflowId};
use aion_store::ActivityStreamKey;
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::ServerState;
use crate::stream::gate_transcript_workflow;

/// The transcript-fetch request body: the target stream's full identity plus
/// the namespace the workflow runs under (the auth scope), and an optional
/// resume cursor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TranscriptFetchRequest {
    /// The namespace the target workflow runs under (the auth scope).
    pub namespace: String,
    /// The target workflow.
    pub workflow_id: WorkflowId,
    /// The target activity within the workflow.
    pub activity_id: ActivityId,
    /// The target attempt — the third stream axis.
    pub attempt: u32,
    /// Read records with `store_seq >= from_seq`; omitted reads the whole
    /// retained transcript from `store_seq == 0`.
    #[serde(default)]
    pub from_seq: Option<u64>,
}

/// The transcript-fetch response body: the retained events, in `store_seq`
/// order, each carrying its `store_seq`. Empty for a stream with nothing
/// retained (an unknown or pre-retention run).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TranscriptFetchResponse {
    /// The retained transcript events in `store_seq` order.
    pub events: Vec<ActivityEvent>,
}

/// The stream-enumeration request body: the workflow whose retained transcript
/// streams to list, plus the namespace it runs under (the auth scope).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TranscriptStreamsRequest {
    /// The namespace the target workflow runs under (the auth scope).
    pub namespace: String,
    /// The workflow whose retained transcript streams to enumerate.
    pub workflow_id: WorkflowId,
}

/// One retained transcript stream of the workflow.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TranscriptStreamEntry {
    /// The activity within the workflow.
    pub activity_id: ActivityId,
    /// The attempt number — the third stream axis.
    pub attempt: u32,
    /// Next `store_seq` to be written == count of retained records.
    pub head: u64,
}

/// The stream-enumeration response body: every retained stream, ordered by
/// `(activity, attempt)`. Empty for a workflow with no retained transcript.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TranscriptStreamsResponse {
    /// The workflow's retained transcript streams.
    pub streams: Vec<TranscriptStreamEntry>,
}

/// `POST /workflows/transcript`.
///
/// Namespace-gates the caller (byte-identical to the transcript WS
/// subscription), then reads the retained `O` tail of the addressed stream
/// from `from_seq` (default `0`). An unknown or pre-retention stream returns
/// `200 { "events": [] }` — the honest answer, never an error; only an
/// authorization failure or a store fault is an HTTP error.
pub(crate) async fn fetch_transcript(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<TranscriptFetchRequest>,
) -> Result<Json<TranscriptFetchResponse>, HttpWireError> {
    gate_transcript_workflow(&state, &caller, &request.namespace, &request.workflow_id)
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    let key = ActivityStreamKey::new(request.workflow_id, request.activity_id, request.attempt);
    let records = state
        .transcript_publisher()
        .replay_from(&key, request.from_seq.unwrap_or(0))
        .await
        .map_err(|error| HttpWireError(crate::ServerError::from(error).to_wire_error()))?;
    Ok(Json(TranscriptFetchResponse {
        events: records.into_iter().map(|record| record.event).collect(),
    }))
}

/// `POST /workflows/transcripts`.
///
/// Namespace-gates the caller (the same per-workflow gate), then enumerates
/// the workflow's retained transcript streams from the durable `O` keyspace.
/// A workflow with no retained transcript answers `200 { "streams": [] }` —
/// old runs simply have none.
pub(crate) async fn list_transcript_streams(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<TranscriptStreamsRequest>,
) -> Result<Json<TranscriptStreamsResponse>, HttpWireError> {
    gate_transcript_workflow(&state, &caller, &request.namespace, &request.workflow_id)
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    let summaries = state
        .transcript_publisher()
        .list_streams(&request.workflow_id)
        .await
        .map_err(|error| HttpWireError(crate::ServerError::from(error).to_wire_error()))?;
    Ok(Json(TranscriptStreamsResponse {
        streams: summaries
            .into_iter()
            .map(|summary| TranscriptStreamEntry {
                activity_id: summary.key.activity_id,
                attempt: summary.key.attempt,
                head: summary.head,
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityEventKind, MessageRole};
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    /// The fetch request carries the full stream identity + auth scope and
    /// round-trips through serde; `from_seq` is optional and defaults absent.
    #[test]
    fn fetch_request_round_trips_and_from_seq_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let request = TranscriptFetchRequest {
            namespace: "tenant-a".to_owned(),
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(3),
            attempt: 1,
            from_seq: Some(7),
        };
        let json = serde_json::to_string(&request)?;
        let decoded: TranscriptFetchRequest = serde_json::from_str(&json)?;
        assert_eq!(decoded.namespace, "tenant-a");
        assert_eq!(decoded.attempt, 1);
        assert_eq!(decoded.from_seq, Some(7));
        // `activity_id` crosses the wire as a plain number.
        let value: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(value["activity_id"], serde_json::json!(3));

        // Omitting from_seq decodes to None (read the whole transcript).
        let minimal: TranscriptFetchRequest = serde_json::from_value(serde_json::json!({
            "namespace": "tenant-a",
            "workflow_id": WorkflowId::new(Uuid::nil()),
            "activity_id": 3,
            "attempt": 0,
        }))?;
        assert_eq!(minimal.from_seq, None);
        Ok(())
    }

    /// The fetch response carries the retained events (with their stamped
    /// `store_seq`) and round-trips, including the empty honest answer.
    #[test]
    fn fetch_response_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let response = TranscriptFetchResponse {
            events: vec![ActivityEvent {
                workflow_id: WorkflowId::new(Uuid::nil()),
                activity_id: ActivityId::from_sequence_position(3),
                attempt: 0,
                agent_id: Uuid::nil(),
                agent_role: "operator".to_owned(),
                emitted_at: Utc::now(),
                worker_seq: 0,
                store_seq: Some(4),
                ephemeral: false,
                kind: ActivityEventKind::Message {
                    role: MessageRole::User,
                    text: "steer".to_owned(),
                },
            }],
        };
        let json = serde_json::to_string(&response)?;
        let decoded: TranscriptFetchResponse = serde_json::from_str(&json)?;
        assert_eq!(decoded.events.len(), 1);
        assert_eq!(decoded.events[0].store_seq, Some(4));

        let empty: TranscriptFetchResponse = serde_json::from_str(r#"{"events":[]}"#)?;
        assert!(empty.events.is_empty());
        Ok(())
    }

    /// The enumeration request/response round-trip, including the empty list
    /// for a workflow with no retained transcript.
    #[test]
    fn streams_request_and_response_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let request = TranscriptStreamsRequest {
            namespace: "tenant-a".to_owned(),
            workflow_id: WorkflowId::new(Uuid::nil()),
        };
        let json = serde_json::to_string(&request)?;
        let decoded: TranscriptStreamsRequest = serde_json::from_str(&json)?;
        assert_eq!(decoded.namespace, "tenant-a");

        let response = TranscriptStreamsResponse {
            streams: vec![TranscriptStreamEntry {
                activity_id: ActivityId::from_sequence_position(3),
                attempt: 0,
                head: 5,
            }],
        };
        let json = serde_json::to_string(&response)?;
        let decoded: TranscriptStreamsResponse = serde_json::from_str(&json)?;
        assert_eq!(decoded.streams.len(), 1);
        assert_eq!(decoded.streams[0].head, 5);
        let value: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(value["streams"][0]["activity_id"], serde_json::json!(3));

        let empty: TranscriptStreamsResponse = serde_json::from_str(r#"{"streams":[]}"#)?;
        assert!(empty.streams.is_empty());
        Ok(())
    }
}
