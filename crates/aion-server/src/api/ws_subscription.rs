//! WebSocket subscription request reading and JSON decoding.
//!
//! The first client frame on `/events/stream` is a JSON `SubscriptionRequest`.
//! This module owns the tolerant decode of that frame: the canonical proto
//! serde shape is accepted first, then the documented hand-written shapes
//! (`per_workflow` / `filtered` / `firehose`, optionally wrapped in
//! `{"subscription": ...}`).

use aion_proto::{
    ClusterSubscription, FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription,
    ProtoActivityId, ProtoWorkflowId, SubscriptionRequest, TranscriptSubscription, WireError,
    subscription_request,
};
use axum::extract::ws::{Message, WebSocket};
use serde_json::{Map, Value};

use crate::error::ServerError;

/// Read the first subscription frame from an accepted WebSocket.
///
/// Ping/pong frames are ignored while waiting. A malformed frame or a socket
/// error is a decode failure the caller reports as one terminal error frame.
///
/// A clean close *before* any subscribe frame arrives — either an explicit
/// `Close` frame or the stream ending (`recv` returns `None`) — is NOT an error:
/// it is the normal lifecycle of a client that disconnects before subscribing
/// (e.g. a React `StrictMode` double-mount whose first socket is torn down before
/// it can send, or a page navigated away during the handshake). It is returned
/// as `Ok(None)` so the caller ends the connection gracefully without logging a
/// spurious warning or trying to write an error frame to an already-closing
/// socket.
///
/// # Errors
///
/// Returns [`ServerError::Wire`] (`invalid_input`) only when the request frame
/// is present but cannot be read or decoded.
pub async fn read_subscription_request(
    socket: &mut WebSocket,
) -> Result<Option<SubscriptionRequest>, ServerError> {
    loop {
        // Stream ended before any subscribe frame: a benign pre-subscribe close.
        let Some(message) = socket.recv().await else {
            return Ok(None);
        };
        let message = message.map_err(|source| {
            WireError::invalid_input(format!(
                "failed to read websocket subscription request: {source}"
            ))
        })?;

        match message {
            Message::Text(text) => return decode_subscription_request(text.as_bytes()).map(Some),
            Message::Binary(bytes) => return decode_subscription_request(&bytes).map(Some),
            Message::Ping(_) | Message::Pong(_) => {}
            // Explicit clean close before subscribing: benign, not an error.
            Message::Close(_) => return Ok(None),
        }
    }
}

/// Decode a subscription request from raw frame bytes.
///
/// # Errors
///
/// Returns [`ServerError::Wire`] (`invalid_input`) when the bytes are not a
/// recognizable subscription JSON object.
pub fn decode_subscription_request(bytes: &[u8]) -> Result<SubscriptionRequest, ServerError> {
    let value = serde_json::from_slice::<Value>(bytes).map_err(|source| {
        WireError::invalid_input(format!("invalid websocket subscription JSON: {source}"))
    })?;
    decode_subscription_value(&value)
}

fn decode_subscription_value(value: &Value) -> Result<SubscriptionRequest, ServerError> {
    if let Ok(request) = serde_json::from_value::<SubscriptionRequest>(value.clone()) {
        if request.subscription.is_some() {
            return Ok(request);
        }
    }

    let subscription = value.get("subscription").unwrap_or(value);
    let Some(subscription) = subscription.as_object() else {
        return Err(
            WireError::invalid_input("websocket subscription must be a JSON object").into(),
        );
    };

    if let Some(value) = subscription.get("per_workflow") {
        return Ok(SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::PerWorkflow(
                decode_per_workflow_subscription(value)?,
            )),
        });
    }
    if let Some(value) = subscription.get("filtered") {
        return Ok(SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Filtered(
                decode_filtered_subscription(value)?,
            )),
        });
    }
    if let Some(value) = subscription.get("firehose") {
        return Ok(SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Firehose(
                decode_firehose_subscription(value)?,
            )),
        });
    }
    if let Some(value) = subscription.get("cluster") {
        return Ok(SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Cluster(
                decode_cluster_subscription(value)?,
            )),
        });
    }
    if let Some(value) = subscription.get("transcript") {
        return Ok(SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Transcript(
                decode_transcript_subscription(value)?,
            )),
        });
    }

    Err(WireError::invalid_input(
        "websocket subscription must contain per_workflow, filtered, firehose, cluster, or transcript",
    )
    .into())
}

fn decode_per_workflow_subscription(value: &Value) -> Result<PerWorkflowSubscription, ServerError> {
    let object = subscription_object(value, "per-workflow")?;
    Ok(PerWorkflowSubscription {
        namespace: required_string(object, "namespace", "per-workflow subscription")?.to_owned(),
        workflow_id: Some(decode_workflow_id_value(
            object.get("workflow_id").ok_or_else(|| {
                WireError::invalid_input("per-workflow subscription requires workflow_id")
            })?,
        )?),
        resume_from_seq: decode_resume_from_seq(object)?,
    })
}

/// Decode the optional resume cursor. Presence only: range validation against
/// the recorded history head happens after the namespace guard verdict, in
/// `stream::resume`, so decoding can never leak existence information.
fn decode_resume_from_seq(object: &Map<String, Value>) -> Result<Option<u64>, ServerError> {
    match object.get("resume_from_seq") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            WireError::invalid_input(
                "per-workflow subscription resume_from_seq must be an unsigned integer",
            )
            .into()
        }),
    }
}

fn decode_filtered_subscription(value: &Value) -> Result<FilteredSubscription, ServerError> {
    let object = subscription_object(value, "filtered")?;
    let status = match object.get("status") {
        Some(Value::String(status)) => Some(decode_status_name(status)?),
        Some(Value::Number(status)) => status.as_i64().and_then(|value| i32::try_from(value).ok()),
        Some(Value::Null) | None => None,
        Some(_other) => None,
    };
    Ok(FilteredSubscription {
        namespace: required_string(object, "namespace", "filtered subscription")?.to_owned(),
        workflow_type: object
            .get("workflow_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        status,
        namespace_selector: object
            .get("namespace_selector")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Decode the WS3 cluster subscription. The only field is the optional
/// `after_seq` resume cursor (presence-only; `0`/absent both request the full
/// in-flight backlog). The cluster subscription carries no namespace — it is
/// deployment-scoped and authorized by the caller's deploy grant.
fn decode_cluster_subscription(value: &Value) -> Result<ClusterSubscription, ServerError> {
    // An empty object `{}` is valid (after_seq defaults to 0). A bare absent
    // value object is also accepted.
    let after_seq = match value.as_object().and_then(|object| object.get("after_seq")) {
        None | Some(Value::Null) => 0,
        Some(seq) => seq.as_u64().ok_or_else(|| {
            WireError::invalid_input("cluster subscription after_seq must be an unsigned integer")
        })?,
    };
    Ok(ClusterSubscription { after_seq })
}

/// Decode the NOI-5b transcript subscription. Namespace-scoped like the
/// per-workflow arm: it carries `namespace` + `workflow_id`, plus the
/// `activity_id`/`attempt` axes that pin the `O`-keyspace stream, and an optional
/// `after_seq` resume cursor (presence-only; range/validity is irrelevant since
/// `store_seq` is a durable read that returns an empty tail past the head).
fn decode_transcript_subscription(value: &Value) -> Result<TranscriptSubscription, ServerError> {
    let object = subscription_object(value, "transcript")?;
    let workflow_id = decode_workflow_id_value(object.get("workflow_id").ok_or_else(|| {
        WireError::invalid_input("transcript subscription requires workflow_id")
    })?)?;
    let activity_id = decode_activity_id_value(object.get("activity_id").ok_or_else(|| {
        WireError::invalid_input("transcript subscription requires activity_id")
    })?)?;
    let attempt = decode_attempt(object)?;
    Ok(TranscriptSubscription {
        namespace: required_string(object, "namespace", "transcript subscription")?.to_owned(),
        workflow_id: Some(workflow_id),
        activity_id: Some(activity_id),
        attempt,
        after_seq: decode_after_seq(object)?,
    })
}

/// Decode the transcript `activity_id`: either a bare unsigned sequence position
/// or the structured `{ "sequence_position": n }` proto shape.
fn decode_activity_id_value(value: &Value) -> Result<ProtoActivityId, ServerError> {
    if let Some(sequence_position) = value.as_u64() {
        return Ok(ProtoActivityId { sequence_position });
    }
    serde_json::from_value::<ProtoActivityId>(value.clone()).map_err(|source| {
        WireError::invalid_input(format!(
            "invalid transcript subscription activity_id: {source}"
        ))
        .into()
    })
}

/// Decode the required transcript `attempt` axis as a `u32`.
fn decode_attempt(object: &Map<String, Value>) -> Result<u32, ServerError> {
    match object.get("attempt") {
        // Absent attempt defaults to 0 (the first attempt's stream), matching the
        // envelope's default attempt for a not-yet-retried activity.
        None | Some(Value::Null) => Ok(0),
        Some(value) => value
            .as_u64()
            .and_then(|attempt| u32::try_from(attempt).ok())
            .ok_or_else(|| {
                WireError::invalid_input(
                    "transcript subscription attempt must be an unsigned 32-bit integer",
                )
                .into()
            }),
    }
}

/// Decode the optional transcript resume cursor `after_seq` (presence-only).
fn decode_after_seq(object: &Map<String, Value>) -> Result<Option<u64>, ServerError> {
    match object.get("after_seq") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            WireError::invalid_input(
                "transcript subscription after_seq must be an unsigned integer",
            )
            .into()
        }),
    }
}

fn decode_firehose_subscription(value: &Value) -> Result<FirehoseSubscription, ServerError> {
    let object = subscription_object(value, "firehose")?;
    let namespace = object
        .get("namespace")
        .or_else(|| object.get("namespace_selector"))
        .and_then(Value::as_str)
        .ok_or_else(|| WireError::invalid_input("firehose subscription requires namespace"))?;
    Ok(FirehoseSubscription {
        namespace: namespace.to_owned(),
    })
}

fn subscription_object<'a>(
    value: &'a Value,
    subscription_name: &str,
) -> Result<&'a Map<String, Value>, ServerError> {
    value.as_object().ok_or_else(|| {
        WireError::invalid_input(format!(
            "{subscription_name} subscription must be a JSON object"
        ))
        .into()
    })
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<&'a str, ServerError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| WireError::invalid_input(format!("{context} requires {key}")).into())
}

fn decode_workflow_id_value(value: &Value) -> Result<ProtoWorkflowId, ServerError> {
    if let Some(uuid) = value.as_str() {
        return Ok(ProtoWorkflowId {
            uuid: uuid.to_owned(),
        });
    }
    serde_json::from_value::<ProtoWorkflowId>(value.clone()).map_err(|source| {
        WireError::invalid_input(format!(
            "invalid per-workflow subscription workflow_id: {source}"
        ))
        .into()
    })
}

fn decode_status_name(status: &str) -> Result<i32, ServerError> {
    match status {
        "running" | "Running" => Ok(aion_proto::ProtoWorkflowStatus::Running as i32),
        "completed" | "Completed" => Ok(aion_proto::ProtoWorkflowStatus::Completed as i32),
        "failed" | "Failed" => Ok(aion_proto::ProtoWorkflowStatus::Failed as i32),
        "cancelled" | "Cancelled" | "canceled" | "Canceled" => {
            Ok(aion_proto::ProtoWorkflowStatus::Cancelled as i32)
        }
        "timed_out" | "TimedOut" => Ok(aion_proto::ProtoWorkflowStatus::TimedOut as i32),
        "continued_as_new" | "ContinuedAsNew" => {
            Ok(aion_proto::ProtoWorkflowStatus::ContinuedAsNew as i32)
        }
        other => Err(WireError::invalid_input(format!(
            "invalid workflow status in websocket subscription: {other}"
        ))
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use aion_proto::{WireErrorCode, subscription_request};
    use serde_json::json;

    use super::decode_subscription_request;
    use crate::error::ServerError;

    fn decode(value: &serde_json::Value) -> Result<aion_proto::SubscriptionRequest, ServerError> {
        decode_subscription_request(value.to_string().as_bytes())
    }

    fn per_workflow(
        request: aion_proto::SubscriptionRequest,
    ) -> Result<aion_proto::PerWorkflowSubscription, Box<dyn std::error::Error>> {
        match request.subscription {
            Some(subscription_request::Subscription::PerWorkflow(subscription)) => Ok(subscription),
            other => Err(format!("expected a per-workflow subscription, got {other:?}").into()),
        }
    }

    #[test]
    fn per_workflow_resume_from_seq_is_decoded() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = uuid::Uuid::from_u128(7).to_string();
        let request = decode(&json!({
            "per_workflow": {
                "namespace": "tenant-a",
                "workflow_id": workflow_id,
                "resume_from_seq": 42,
            }
        }))?;

        assert_eq!(per_workflow(request)?.resume_from_seq, Some(42));
        Ok(())
    }

    #[test]
    fn per_workflow_resume_from_seq_zero_passes_decode_for_post_guard_validation()
    -> Result<(), Box<dyn std::error::Error>> {
        // Decode is presence-only; the 0-is-invalid range check belongs after
        // the namespace guard so probes can never distinguish existence.
        let request = decode(&json!({
            "per_workflow": {
                "namespace": "tenant-a",
                "workflow_id": uuid::Uuid::from_u128(7).to_string(),
                "resume_from_seq": 0,
            }
        }))?;

        assert_eq!(per_workflow(request)?.resume_from_seq, Some(0));
        Ok(())
    }

    #[test]
    fn per_workflow_resume_from_seq_absent_or_null_is_none()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = uuid::Uuid::from_u128(7).to_string();
        let absent = decode(&json!({
            "per_workflow": { "namespace": "tenant-a", "workflow_id": workflow_id }
        }))?;
        let null = decode(&json!({
            "per_workflow": {
                "namespace": "tenant-a",
                "workflow_id": workflow_id,
                "resume_from_seq": null,
            }
        }))?;

        assert_eq!(per_workflow(absent)?.resume_from_seq, None);
        assert_eq!(per_workflow(null)?.resume_from_seq, None);
        Ok(())
    }

    #[test]
    fn per_workflow_resume_from_seq_rejects_non_unsigned_values() {
        for bad in [json!(-1), json!(1.5), json!("7")] {
            let result = decode(&json!({
                "per_workflow": {
                    "namespace": "tenant-a",
                    "workflow_id": uuid::Uuid::from_u128(7).to_string(),
                    "resume_from_seq": bad,
                }
            }));
            let error = result.err().map(|error| error.to_wire_error());
            assert_eq!(
                error.as_ref().map(|error| error.code),
                Some(WireErrorCode::InvalidInput),
                "expected invalid_input, got {error:?}"
            );
        }
    }

    #[test]
    fn wrapped_subscription_shape_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
        let request = decode(&json!({
            "subscription": {
                "per_workflow": {
                    "namespace": "tenant-a",
                    "workflow_id": { "uuid": uuid::Uuid::from_u128(7).to_string() },
                    "resume_from_seq": 3,
                }
            }
        }))?;

        let subscription = per_workflow(request)?;
        assert_eq!(subscription.namespace, "tenant-a");
        assert_eq!(subscription.resume_from_seq, Some(3));
        Ok(())
    }

    #[test]
    fn filtered_and_firehose_shapes_still_decode() -> Result<(), Box<dyn std::error::Error>> {
        let filtered = decode(&json!({
            "filtered": { "namespace": "tenant-a", "status": "Completed" }
        }))?;
        let firehose = decode(&json!({ "firehose": { "namespace": "tenant-a" } }))?;

        assert!(matches!(
            filtered.subscription,
            Some(subscription_request::Subscription::Filtered(_))
        ));
        assert!(matches!(
            firehose.subscription,
            Some(subscription_request::Subscription::Firehose(_))
        ));
        Ok(())
    }

    /// NOI-5b: the transcript arm decodes its namespace + workflow/activity/
    /// attempt axes and the optional `after_seq` cursor. A bare unsigned
    /// `activity_id` and an absent `attempt` (defaulting to 0) are both accepted.
    #[test]
    fn transcript_subscription_decodes_axes_and_cursor() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = uuid::Uuid::from_u128(7).to_string();
        let request = decode(&json!({
            "transcript": {
                "namespace": "tenant-a",
                "workflow_id": workflow_id,
                "activity_id": 3,
                "attempt": 2,
                "after_seq": 5,
            }
        }))?;
        let Some(subscription_request::Subscription::Transcript(transcript)) = request.subscription
        else {
            return Err("expected a transcript subscription".into());
        };
        assert_eq!(transcript.namespace, "tenant-a");
        assert_eq!(
            transcript.activity_id.map(|id| id.sequence_position),
            Some(3)
        );
        assert_eq!(transcript.attempt, 2);
        assert_eq!(transcript.after_seq, Some(5));
        Ok(())
    }

    #[test]
    fn transcript_subscription_defaults_attempt_and_cursor()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = decode(&json!({
            "transcript": {
                "namespace": "tenant-a",
                "workflow_id": uuid::Uuid::from_u128(7).to_string(),
                "activity_id": 3,
            }
        }))?;
        let Some(subscription_request::Subscription::Transcript(transcript)) = request.subscription
        else {
            return Err("expected a transcript subscription".into());
        };
        assert_eq!(transcript.attempt, 0, "absent attempt defaults to 0");
        assert_eq!(
            transcript.after_seq, None,
            "absent after_seq is a fresh subscriber"
        );
        Ok(())
    }

    #[test]
    fn transcript_subscription_requires_activity_id() {
        let error = decode(&json!({
            "transcript": {
                "namespace": "tenant-a",
                "workflow_id": uuid::Uuid::from_u128(7).to_string(),
            }
        }))
        .err()
        .map(|error| error.to_wire_error());
        assert_eq!(
            error.map(|error| error.code),
            Some(WireErrorCode::InvalidInput)
        );
    }

    #[test]
    fn unknown_subscription_shape_is_invalid_input() {
        let error = decode(&json!({ "mystery": {} }))
            .err()
            .map(|error| error.to_wire_error());
        assert_eq!(
            error.map(|error| error.code),
            Some(WireErrorCode::InvalidInput)
        );
    }
}
