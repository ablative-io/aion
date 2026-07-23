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
use aion_proto::WireError;
use aion_store::{ActivityRecord, ActivityStreamKey};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::ServerState;
use crate::stream::gate_transcript_workflow;

const MAX_WINDOW_LIMIT: u32 = 2_000;

/// The transcript-fetch request body: the target stream's full identity plus
/// the namespace the workflow runs under (the auth scope), an optional resume
/// cursor, and optional bounded-window selectors.
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
    /// Maximum records to return. Omitted preserves the unbounded legacy read.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Return only the last `n` retained records. Mutually exclusive with
    /// `from_seq`.
    #[serde(default)]
    pub last: Option<u32>,
}

/// The transcript-fetch response body: the retained events, in `store_seq`
/// order, each carrying its `store_seq`. Empty for a stream with nothing
/// retained (an unknown or pre-retention run).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TranscriptFetchResponse {
    /// The retained transcript events in `store_seq` order.
    pub events: Vec<ActivityEvent>,
    /// First omitted `store_seq` when `limit` truncated the response.
    pub next_from_seq: Option<u64>,
    /// Stream head at read time: the next durable `store_seq`.
    pub head_seq: u64,
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
/// `200` with empty `events`, a null `next_from_seq`, and `head_seq == 0` — the
/// honest answer, never an error; only an authorization failure or a store fault
/// is an HTTP error.
pub(crate) async fn fetch_transcript(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<TranscriptFetchRequest>,
) -> Result<Json<TranscriptFetchResponse>, HttpWireError> {
    gate_transcript_workflow(&state, &caller, &request.namespace, &request.workflow_id)
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    if request.from_seq.is_some() && request.last.is_some() {
        return Err(HttpWireError(WireError::invalid_input(
            "transcript from_seq and last are mutually exclusive",
        )));
    }
    let key = ActivityStreamKey::new(request.workflow_id, request.activity_id, request.attempt);
    let from_seq = request.from_seq.map_or(0, |seq| seq);
    let read_from = request.last.map_or(from_seq, |_last| 0);
    let ranged = state
        .transcript_publisher()
        .replay_from(&key, read_from)
        .await
        .map_err(|error| HttpWireError(crate::ServerError::from(error).to_wire_error()))?;
    let mut records = if ranged.is_empty() && read_from != 0 {
        state
            .transcript_publisher()
            .replay_from(&key, 0)
            .await
            .map_err(|error| HttpWireError(crate::ServerError::from(error).to_wire_error()))?
    } else {
        ranged
    };
    let head_seq = transcript_head(&records)?;
    if request.last.is_none() && read_from != 0 {
        records.retain(|record| record.store_seq >= read_from);
    }
    if let Some(last) = request.last {
        let last = usize::try_from(last).map_err(|error| {
            HttpWireError(WireError::invalid_input(format!(
                "transcript last is too large: {error}"
            )))
        })?;
        let first = records.len().saturating_sub(last);
        drop(records.drain(..first));
    }
    let limit = request
        .limit
        .map(|limit| limit.clamp(1, MAX_WINDOW_LIMIT) as usize);
    let next_from_seq = limit.and_then(|limit| records.get(limit).map(|record| record.store_seq));
    if let Some(limit) = limit {
        records.truncate(limit);
    }
    Ok(Json(TranscriptFetchResponse {
        events: records.into_iter().map(|record| record.event).collect(),
        next_from_seq,
        head_seq,
    }))
}

fn transcript_head(records: &[ActivityRecord]) -> Result<u64, HttpWireError> {
    records.last().map_or(Ok(0), |record| {
        record.store_seq.checked_add(1).ok_or_else(|| {
            HttpWireError(WireError::backend(
                "transcript store_seq exhausted the u64 sequence space",
            ))
        })
    })
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The fetch request carries the full stream identity + auth scope and
    /// round-trips through serde; `from_seq` is optional and defaults absent.
    #[test]
    fn fetch_request_round_trips_and_from_seq_defaults() -> TestResult {
        let request = TranscriptFetchRequest {
            namespace: "tenant-a".to_owned(),
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(3),
            attempt: 1,
            from_seq: Some(7),
            limit: Some(25),
            last: None,
        };
        let json = serde_json::to_string(&request)?;
        let decoded: TranscriptFetchRequest = serde_json::from_str(&json)?;
        assert_eq!(decoded.namespace, "tenant-a");
        assert_eq!(decoded.attempt, 1);
        assert_eq!(decoded.from_seq, Some(7));
        assert_eq!(decoded.limit, Some(25));
        assert_eq!(decoded.last, None);
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
        assert_eq!(minimal.limit, None);
        assert_eq!(minimal.last, None);
        Ok(())
    }

    /// The fetch response carries the retained events (with their stamped
    /// `store_seq`) and round-trips, including the empty honest answer.
    #[test]
    fn fetch_response_round_trips() -> TestResult {
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
            next_from_seq: Some(5),
            head_seq: 8,
        };
        let json = serde_json::to_string(&response)?;
        let decoded: TranscriptFetchResponse = serde_json::from_str(&json)?;
        assert_eq!(decoded.events.len(), 1);
        assert_eq!(decoded.events[0].store_seq, Some(4));
        assert_eq!(decoded.next_from_seq, Some(5));
        assert_eq!(decoded.head_seq, 8);

        let empty: TranscriptFetchResponse =
            serde_json::from_str(r#"{"events":[],"next_from_seq":null,"head_seq":0}"#)?;
        assert!(empty.events.is_empty());
        Ok(())
    }

    /// The enumeration request/response round-trip, including the empty list
    /// for a workflow with no retained transcript.
    #[test]
    fn streams_request_and_response_round_trip() -> TestResult {
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
