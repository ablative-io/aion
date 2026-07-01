//! The stdout demux — the pure translation from a plain CLI agent's interleaved stdout lines into
//! neutral [`ActivityEvent`]s.
//!
//! This is the case-(b) analogue of the Norn adapter's `translate` module, and the whole point of
//! the observability-only shape: a plain-stdout agent has **no** structured channel, so its
//! transcript is recovered by parsing the interleaved lines it prints. The demux is deliberately
//! **lossless**: any line the classifier does not recognise falls through to
//! [`ActivityEventKind::Raw`] carrying the verbatim line, so nothing an agent prints is ever
//! silently dropped.
//!
//! # The recognised shape
//!
//! A line that parses as a JSON object with a `"type"` string is mapped onto a neutral kind:
//!
//! | line `type` | neutral kind |
//! |---|---|
//! | `"message"` (with `"text"`) | [`ActivityEventKind::Message`] (assistant role) |
//! | `"tool_call"` | [`ActivityEventKind::ToolCall`] |
//! | `"tool_result"` | [`ActivityEventKind::ToolResult`] |
//! | `"stop"` (with `"reason"`) | [`ActivityEventKind::Stop`] |
//!
//! Everything else — a non-JSON log line, a JSON value without a known `type`, an empty line — is a
//! [`ActivityEventKind::Raw`]. This mirrors real observability-only CLIs, which interleave a little
//! structured output with free-form logging.
//!
//! The demux is pure and independently tested; the session ([`crate::session`]) is the only place
//! that reads the child's stdout and stamps ordering onto the events this produces.

use aion_core::{ActivityEvent, ActivityEventKind, MessageRole, StopKind};
use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

/// The `(workflow, activity, attempt)` identity stamped onto every demuxed event.
///
/// The plain-stdout agent carries no agent sub-identity, so [`ActivityEvent::agent_id`] is always
/// the nil UUID and [`ActivityEvent::agent_role`] is a fixed neutral label — an honest reflection
/// of a single-agent CLI with no multi-agent attribution to recover.
#[derive(Clone, Debug)]
pub struct EventIdentity {
    /// The workflow this run belongs to.
    pub workflow_id: aion_core::WorkflowId,
    /// The activity within the workflow.
    pub activity_id: aion_core::ActivityId,
    /// The attempt number of the run.
    pub attempt: u32,
}

/// The fixed agent role for a single-agent plain-stdout CLI (no sub-agent attribution to recover).
const CLI_AGENT_ROLE: &str = "cli";

/// The `source` label a [`ActivityEventKind::Raw`] passthrough carries — the adapter's stdout.
const RAW_SOURCE: &str = "stdout";

/// Maps one line of the child's stdout into a neutral [`ActivityEvent`].
///
/// `worker_seq` is the session's best-effort local monotonic order for the line. A line that parses
/// as a recognised JSON object maps onto the matching kind; anything else falls through to
/// [`ActivityEventKind::Raw`], so no line is ever dropped.
#[must_use]
pub fn line_to_event(identity: &EventIdentity, worker_seq: u64, line: &str) -> ActivityEvent {
    let kind = classify_line(line);
    ActivityEvent {
        workflow_id: identity.workflow_id.clone(),
        activity_id: identity.activity_id.clone(),
        attempt: identity.attempt,
        agent_id: Uuid::nil(),
        agent_role: CLI_AGENT_ROLE.to_owned(),
        emitted_at: Utc::now(),
        worker_seq,
        store_seq: None,
        // A plain-stdout CLI has no token-delta channel, so no event is ever ephemeral.
        ephemeral: false,
        kind,
    }
}

/// Classifies a stdout line into an [`ActivityEventKind`].
///
/// A JSON object with a known `"type"` maps onto the matching kind; every other line (non-JSON, an
/// unknown `type`, a bare value) is a [`ActivityEventKind::Raw`] carrying the line verbatim.
fn classify_line(line: &str) -> ActivityEventKind {
    let Some(object) = serde_json::from_str::<Value>(line)
        .ok()
        .filter(Value::is_object)
    else {
        return raw_kind(line);
    };
    match object.get("type").and_then(Value::as_str) {
        Some("message") => message_kind(&object),
        Some("tool_call") => tool_call_kind(&object),
        Some("tool_result") => tool_result_kind(&object),
        Some("stop") => stop_kind(&object),
        // A JSON object whose `type` this adapter does not map is passed through verbatim, never
        // dropped: the raw value is the parsed object so the console still sees the structure.
        _ => ActivityEventKind::Raw {
            source: RAW_SOURCE.to_owned(),
            value: object,
        },
    }
}

/// A [`ActivityEventKind::Raw`] carrying a stdout line verbatim as a JSON string value.
fn raw_kind(line: &str) -> ActivityEventKind {
    ActivityEventKind::Raw {
        source: RAW_SOURCE.to_owned(),
        value: Value::String(line.to_owned()),
    }
}

/// Reads a string field from a JSON object, or an empty string when absent.
fn string_field(object: &Value, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Builds a [`ActivityEventKind::Message`] from a `{"type":"message","text":...}` line.
///
/// A plain-stdout CLI's structured messages are agent output, so the neutral role is always
/// [`MessageRole::Assistant`].
fn message_kind(object: &Value) -> ActivityEventKind {
    ActivityEventKind::Message {
        role: MessageRole::Assistant,
        text: string_field(object, "text"),
    }
}

/// Builds a [`ActivityEventKind::ToolCall`] from a `{"type":"tool_call",...}` line.
fn tool_call_kind(object: &Value) -> ActivityEventKind {
    ActivityEventKind::ToolCall {
        tool: string_field(object, "tool"),
        call_id: string_field(object, "call_id"),
        input: object.get("input").cloned().unwrap_or(Value::Null),
    }
}

/// Builds a [`ActivityEventKind::ToolResult`] from a `{"type":"tool_result",...}` line.
fn tool_result_kind(object: &Value) -> ActivityEventKind {
    ActivityEventKind::ToolResult {
        call_id: string_field(object, "call_id"),
        output: object.get("output").cloned().unwrap_or(Value::Null),
        is_error: object
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// Builds a [`ActivityEventKind::Stop`] from a `{"type":"stop","reason":...}` line.
fn stop_kind(object: &Value) -> ActivityEventKind {
    let reason = match object.get("reason").and_then(Value::as_str) {
        Some("end_turn") => StopKind::EndTurn,
        Some("tool_use") => StopKind::ToolUse,
        Some("limit_reached") => StopKind::LimitReached,
        Some("cancelled") => StopKind::Cancelled,
        Some(other) => StopKind::Other {
            reason: other.to_owned(),
        },
        None => StopKind::Other {
            reason: "unknown".to_owned(),
        },
    };
    ActivityEventKind::Stop { reason }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use aion_core::{ActivityId, WorkflowId};
    use serde_json::json;

    fn identity() -> EventIdentity {
        EventIdentity {
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(5),
            attempt: 2,
        }
    }

    #[test]
    fn message_line_maps_to_message_event() {
        let line = json!({ "type": "message", "text": "working on it" }).to_string();
        let event = line_to_event(&identity(), 0, &line);
        assert_eq!(event.attempt, 2);
        assert_eq!(event.agent_role, "cli");
        assert_eq!(event.agent_id, Uuid::nil());
        assert!(!event.ephemeral);
        match event.kind {
            ActivityEventKind::Message { role, text } => {
                assert_eq!(role, MessageRole::Assistant);
                assert_eq!(text, "working on it");
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_line_maps_to_tool_call_event() {
        let line = json!({
            "type": "tool_call",
            "tool": "search",
            "call_id": "c1",
            "input": { "query": "x" },
        })
        .to_string();
        let event = line_to_event(&identity(), 1, &line);
        match event.kind {
            ActivityEventKind::ToolCall {
                tool,
                call_id,
                input,
            } => {
                assert_eq!(tool, "search");
                assert_eq!(call_id, "c1");
                assert_eq!(input, json!({ "query": "x" }));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_line_maps_to_tool_result_event() {
        let line = json!({
            "type": "tool_result",
            "call_id": "c1",
            "output": { "hits": 3 },
            "is_error": false,
        })
        .to_string();
        let event = line_to_event(&identity(), 2, &line);
        match event.kind {
            ActivityEventKind::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(output, json!({ "hits": 3 }));
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn stop_line_maps_stop_reasons() {
        for (label, expected) in [
            ("end_turn", StopKind::EndTurn),
            ("tool_use", StopKind::ToolUse),
            ("limit_reached", StopKind::LimitReached),
            ("cancelled", StopKind::Cancelled),
        ] {
            let line = json!({ "type": "stop", "reason": label }).to_string();
            let event = line_to_event(&identity(), 3, &line);
            match event.kind {
                ActivityEventKind::Stop { reason } => assert_eq!(reason, expected),
                other => panic!("expected Stop, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_stop_reason_falls_through_to_other() {
        let line = json!({ "type": "stop", "reason": "weird" }).to_string();
        let event = line_to_event(&identity(), 4, &line);
        match event.kind {
            ActivityEventKind::Stop {
                reason: StopKind::Other { reason },
            } => assert_eq!(reason, "weird"),
            other => panic!("expected Stop::Other, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_log_line_falls_through_to_raw_verbatim() {
        let line = "  [info] starting the run, reading config...  ";
        let event = line_to_event(&identity(), 5, line);
        match event.kind {
            ActivityEventKind::Raw { source, value } => {
                assert_eq!(source, "stdout");
                assert_eq!(value, Value::String(line.to_owned()));
            }
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn a_json_object_with_unknown_type_is_raw_but_keeps_structure() {
        let line = json!({ "type": "heartbeat", "seq": 4 }).to_string();
        let event = line_to_event(&identity(), 6, &line);
        match event.kind {
            ActivityEventKind::Raw { source, value } => {
                assert_eq!(source, "stdout");
                assert_eq!(value["type"], json!("heartbeat"));
                assert_eq!(value["seq"], json!(4));
            }
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_json_value_is_raw_not_mapped() {
        // A JSON number/array/string is not an object, so it is Raw (a string of the verbatim line).
        let line = "[1, 2, 3]";
        let event = line_to_event(&identity(), 7, line);
        match event.kind {
            ActivityEventKind::Raw { value, .. } => {
                assert_eq!(value, Value::String(line.to_owned()));
            }
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn worker_seq_is_carried_onto_the_event() {
        let event = line_to_event(&identity(), 42, "anything");
        assert_eq!(event.worker_seq, 42);
    }
}
