//! Windowed workflow-history reads for the ops console.

use std::sync::Arc;

use aion::Engine;
use aion_core::{Event, WorkflowId};
use aion_proto::{ProtoDescribeWorkflowRequest, ProtoWorkflowId, WireError};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::{NamespaceOperation, ServerError, ServerState, WorkflowTarget};

const DEFAULT_HISTORY_LIMIT: u32 = 500;
const MAX_WINDOW_LIMIT: u32 = 2_000;
const DEFAULT_PAYLOAD_LIMIT_BYTES: u32 = 2_048;

/// Request for one ascending workflow-history window.
#[derive(Debug, Deserialize)]
pub(crate) struct HistoryFetchRequest {
    namespace: String,
    workflow_id: String,
    #[serde(default)]
    from_seq: Option<u64>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    payload_limit_bytes: Option<u32>,
}

/// One projected workflow-history window.
#[derive(Debug, Serialize)]
pub(crate) struct HistoryFetchResponse {
    events: Vec<Value>,
    next_from_seq: Option<u64>,
    head_seq: u64,
}

/// Request for one full workflow-history event.
#[derive(Debug, Deserialize)]
pub(crate) struct EventFetchRequest {
    namespace: String,
    workflow_id: String,
    seq: u64,
}

/// One full workflow-history event.
#[derive(Debug, Serialize)]
pub(crate) struct EventFetchResponse {
    event: Event,
}

/// `POST /workflows/history`.
pub(crate) async fn fetch_history(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<HistoryFetchRequest>,
) -> Result<Json<HistoryFetchResponse>, HttpWireError> {
    let workflow_id = parse_workflow_id(&request.workflow_id)?;
    let engine = scoped_engine(
        &state,
        &caller,
        &request.namespace,
        &request.workflow_id,
        &workflow_id,
    )
    .await?;
    let from_seq = request.from_seq.map_or(0, |seq| seq);
    let limit = request
        .limit
        .map_or(DEFAULT_HISTORY_LIMIT, |limit| limit)
        .clamp(1, MAX_WINDOW_LIMIT) as usize;
    let payload_limit = request
        .payload_limit_bytes
        .map_or(DEFAULT_PAYLOAD_LIMIT_BYTES, |limit| limit);
    let ranged = engine
        .store()
        .read_history_from(&workflow_id, from_seq)
        .await
        .map_err(store_error)?;

    // A ranged read beyond the head cannot report the head. Fall back to the
    // complete snapshot only for that empty-tail edge, and project the range
    // from the same snapshot so a concurrent append cannot create a gap.
    let (mut tail, head_seq) = if ranged.is_empty() {
        let mut history = engine
            .store()
            .read_history(&workflow_id)
            .await
            .map_err(store_error)?;
        let head_seq = history.last().map_or(0, Event::seq);
        history.retain(|event| event.seq() >= from_seq);
        (history, head_seq)
    } else {
        let head_seq = ranged.last().map_or(0, Event::seq);
        (ranged, head_seq)
    };

    let next_from_seq = tail.get(limit).map(Event::seq);
    tail.truncate(limit);
    let events = tail
        .into_iter()
        .map(|event| project_event(event, payload_limit))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(HistoryFetchResponse {
        events,
        next_from_seq,
        head_seq,
    }))
}

/// `POST /workflows/event`.
pub(crate) async fn fetch_event(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<EventFetchRequest>,
) -> Result<Json<EventFetchResponse>, HttpWireError> {
    let workflow_id = parse_workflow_id(&request.workflow_id)?;
    let engine = scoped_engine(
        &state,
        &caller,
        &request.namespace,
        &request.workflow_id,
        &workflow_id,
    )
    .await?;
    let event = engine
        .store()
        .read_history_from(&workflow_id, request.seq)
        .await
        .map_err(store_error)?
        .into_iter()
        .find(|event| event.seq() == request.seq)
        .ok_or_else(|| {
            HttpWireError(WireError::not_found(format!(
                "workflow event {} at seq {} was not found",
                request.workflow_id, request.seq
            )))
        })?;

    Ok(Json(EventFetchResponse { event }))
}

async fn scoped_engine(
    state: &ServerState,
    caller: &crate::CallerIdentity,
    namespace: &str,
    workflow_id_wire: &str,
    workflow_id: &WorkflowId,
) -> Result<Arc<Engine>, HttpWireError> {
    let describe = ProtoDescribeWorkflowRequest {
        namespace: namespace.to_owned(),
        workflow_id: Some(ProtoWorkflowId {
            uuid: workflow_id_wire.to_owned(),
        }),
        run_id: None,
        include_history: false,
    };
    let target = WorkflowTarget::workflow(workflow_id);
    let scoped = state
        .namespace_guard()
        .scope(caller, &NamespaceOperation::describe(&describe, target))
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    scoped
        .engine()
        .cloned()
        .map_err(|error| HttpWireError(error.to_wire_error()))
}

fn parse_workflow_id(value: &str) -> Result<WorkflowId, HttpWireError> {
    Uuid::parse_str(value)
        .map(WorkflowId::new)
        .map_err(|_error| HttpWireError(WireError::invalid_input("workflow_id must be a UUID")))
}

fn store_error(error: aion_store::StoreError) -> HttpWireError {
    HttpWireError(ServerError::from(error).to_wire_error())
}

fn project_event(event: Event, payload_limit: u32) -> Result<Value, HttpWireError> {
    let mut value = serde_json::to_value(event).map_err(|error| {
        HttpWireError(WireError::backend(format!(
            "workflow event JSON encoding failed: {error}"
        )))
    })?;
    if payload_limit != 0 {
        elide_payload_bytes(&mut value, u64::from(payload_limit));
    }
    Ok(value)
}

fn elide_payload_bytes(value: &mut Value, limit: u64) {
    match value {
        Value::Array(values) => {
            for value in values {
                elide_payload_bytes(value, limit);
            }
        }
        Value::Object(object) => {
            if is_payload_object(object) {
                if let Some(bytes) = object.get_mut("bytes") {
                    if let Some(size_bytes) = payload_byte_len(bytes).filter(|size| *size > limit) {
                        *bytes = serde_json::json!({
                            "__elided": true,
                            "size_bytes": size_bytes,
                        });
                    }
                }
            } else {
                for value in object.values_mut() {
                    elide_payload_bytes(value, limit);
                }
            }
        }
        _ => {}
    }
}

fn is_payload_object(object: &Map<String, Value>) -> bool {
    object.get("content_type").is_some_and(Value::is_string) && object.contains_key("bytes")
}

fn payload_byte_len(value: &Value) -> Option<u64> {
    value
        .as_array()
        .and_then(|bytes| u64::try_from(bytes.len()).ok())
}
