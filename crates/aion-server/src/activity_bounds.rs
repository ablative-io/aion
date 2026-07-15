//! Transcript retention bounds: per-event size truncation for the durable `O`
//! keyspace.
//!
//! A hostile or merely verbose harness can otherwise grow the retained
//! transcript without limit. [`TranscriptBounds`] carries the two operator
//! knobs (`[observability]` config): a ceiling on one persisted event's
//! serialized size and a ceiling on retained events per
//! `(workflow, activity, attempt)` stream. [`bound_event`] applies the
//! per-event ceiling deterministically BEFORE the durable append, so what is
//! persisted, fanned out live, and replayed later are all the same bounded
//! record. The per-stream cap is enforced by the publisher's sequenced append
//! loop (see `activity_publisher`), which owns the head.

use aion_core::{ActivityEvent, ActivityEventKind, ProgressDetail, StopKind};
use aion_store::StoreError;

/// Operator-tunable transcript retention bounds (see `[observability]` config).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptBounds {
    /// Ceiling on one persisted event's serialized size (bytes).
    pub max_event_bytes: usize,
    /// Ceiling on retained events per `(workflow, activity, attempt)` stream.
    pub max_stream_events: u64,
}

impl Default for TranscriptBounds {
    fn default() -> Self {
        Self {
            max_event_bytes: crate::config::DEFAULT_OBSERVABILITY_MAX_EVENT_BYTES,
            max_stream_events: crate::config::DEFAULT_OBSERVABILITY_MAX_STREAM_EVENTS,
        }
    }
}

/// The serialized size of one event, as it would be persisted.
fn serialized_len(event: &ActivityEvent) -> Result<usize, StoreError> {
    Ok(serde_json::to_vec(event)
        .map_err(|error| StoreError::Serialization(error.to_string()))?
        .len())
}

/// A static tag naming an event kind in truncation diagnostics.
const fn kind_tag(kind: &ActivityEventKind) -> &'static str {
    match kind {
        ActivityEventKind::Message { .. } => "message",
        ActivityEventKind::ToolCall { .. } => "tool_call",
        ActivityEventKind::ToolResult { .. } => "tool_result",
        ActivityEventKind::Progress { .. } => "progress",
        ActivityEventKind::Stop { .. } => "stop",
        ActivityEventKind::Raw { .. } => "raw",
        ActivityEventKind::Delta { .. } => "delta",
    }
}

/// Bound one non-ephemeral event to `max_event_bytes`, deterministically.
///
/// An event already within the ceiling passes through unchanged. An oversized
/// event has its dominant payload reduced per kind (text truncated on a `char`
/// boundary with an explicit marker; a structured JSON value replaced by a
/// truncation stub). If the event is STILL over the ceiling after that
/// (pathological non-text overhead) the kind is replaced wholesale with a
/// `Progress`/`Note` describing the truncation — the retained record always
/// says what happened, and nothing here ever panics.
///
/// # Errors
/// [`StoreError::Serialization`] when the event cannot be serialized to
/// measure it (which would also have failed the durable append).
pub(crate) fn bound_event(
    event: &ActivityEvent,
    max_event_bytes: usize,
) -> Result<ActivityEvent, StoreError> {
    let size = serialized_len(event)?;
    if size <= max_event_bytes {
        return Ok(event.clone());
    }
    // Ephemeral deltas never reach the durable path, so an oversized Delta is
    // unreachable here; pass it through unchanged rather than mangling it.
    if matches!(event.kind, ActivityEventKind::Delta { .. }) {
        return Ok(event.clone());
    }
    let mut reduced = event.clone();
    if let Some(kind) = reduce_kind(&event.kind, size, max_event_bytes) {
        reduced.kind = kind;
        if serialized_len(&reduced)? <= max_event_bytes {
            return Ok(reduced);
        }
    }
    // Pathological fallback: the reduction was unavailable or insufficient.
    reduced.kind = ActivityEventKind::Progress {
        detail: ProgressDetail::Note {
            text: format!(
                "event truncated: {} of {size} bytes exceeded observability.max_event_bytes={max_event_bytes}",
                kind_tag(&event.kind)
            ),
        },
    };
    Ok(reduced)
}

/// Reduce the dominant payload of one oversized kind, or `None` when the kind
/// has no reducible payload (the caller then falls back to the note).
fn reduce_kind(
    kind: &ActivityEventKind,
    size: usize,
    max_event_bytes: usize,
) -> Option<ActivityEventKind> {
    match kind {
        ActivityEventKind::Message { role, text } => Some(ActivityEventKind::Message {
            role: *role,
            text: truncate_text(text, size, max_event_bytes),
        }),
        ActivityEventKind::Progress {
            detail: ProgressDetail::Note { text },
        } => Some(ActivityEventKind::Progress {
            detail: ProgressDetail::Note {
                text: truncate_text(text, size, max_event_bytes),
            },
        }),
        ActivityEventKind::Stop {
            reason: StopKind::Error { message },
        } => Some(ActivityEventKind::Stop {
            reason: StopKind::Error {
                message: truncate_text(message, size, max_event_bytes),
            },
        }),
        ActivityEventKind::Stop {
            reason: StopKind::Other { reason },
        } => Some(ActivityEventKind::Stop {
            reason: StopKind::Other {
                reason: truncate_text(reason, size, max_event_bytes),
            },
        }),
        ActivityEventKind::ToolCall {
            tool,
            call_id,
            input,
        } => Some(ActivityEventKind::ToolCall {
            tool: tool.clone(),
            call_id: call_id.clone(),
            input: truncation_stub(input),
        }),
        ActivityEventKind::ToolResult {
            call_id,
            output,
            is_error,
        } => Some(ActivityEventKind::ToolResult {
            call_id: call_id.clone(),
            output: truncation_stub(output),
            is_error: *is_error,
        }),
        ActivityEventKind::Raw { source, value } => Some(ActivityEventKind::Raw {
            source: source.clone(),
            value: truncation_stub(value),
        }),
        ActivityEventKind::Progress { .. }
        | ActivityEventKind::Stop { .. }
        | ActivityEventKind::Delta { .. } => None,
    }
}

/// The stub a structurally oversized JSON payload is replaced with.
fn truncation_stub(original: &serde_json::Value) -> serde_json::Value {
    let original_bytes = serde_json::to_vec(original).map_or(0, |bytes| bytes.len());
    serde_json::json!({
        "truncated": true,
        "original_bytes": original_bytes,
        "reason": "observability.max_event_bytes",
    })
}

/// Truncate `text` so the whole event fits `max_event_bytes`, appending an
/// explicit truncation marker. `size` is the full event's serialized length,
/// so `size - text.len()` approximates the non-text envelope overhead; the
/// caller re-measures afterwards (escaping can inflate the estimate) and falls
/// back to the note when the result is still over.
fn truncate_text(text: &str, size: usize, max_event_bytes: usize) -> String {
    let overhead = size.saturating_sub(text.len());
    let budget = max_event_bytes.saturating_sub(overhead);
    // A provisional marker sized with the largest possible omitted count bounds
    // the marker's byte length, so the final marker (equal or fewer digits)
    // always fits the reserved space.
    let provisional = truncation_marker(text.len());
    let keep_budget = budget.saturating_sub(provisional.len());
    let kept = truncate_on_char_boundary(text, keep_budget);
    let omitted = text.len().saturating_sub(kept.len());
    format!("{kept}{}", truncation_marker(omitted))
}

/// The human-readable marker appended to truncated text.
fn truncation_marker(omitted: usize) -> String {
    format!(" …[truncated {omitted} bytes by observability.max_event_bytes]")
}

/// The longest prefix of `text` that is at most `max_bytes` long and ends on a
/// `char` boundary (`floor_char_boundary` is nightly-only; walk instead).
fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = 0;
    for (index, character) in text.char_indices() {
        let next = index + character.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    text.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityId, MessageRole, WorkflowId};
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    fn envelope(kind: ActivityEventKind) -> ActivityEvent {
        ActivityEvent {
            workflow_id: WorkflowId::new(Uuid::from_u128(1)),
            activity_id: ActivityId::from_sequence_position(3),
            attempt: 0,
            agent_id: Uuid::from_u128(9),
            agent_role: "orchestrator".to_owned(),
            emitted_at: Utc::now(),
            worker_seq: 1,
            store_seq: None,
            ephemeral: false,
            kind,
        }
    }

    fn message(text: &str) -> ActivityEvent {
        envelope(ActivityEventKind::Message {
            role: MessageRole::Assistant,
            text: text.to_owned(),
        })
    }

    #[test]
    fn undersized_event_passes_through_unchanged() -> Result<(), StoreError> {
        let event = message("small");
        let bounded = bound_event(&event, 64 * 1024)?;
        assert_eq!(bounded, event);
        Ok(())
    }

    #[test]
    fn message_text_truncates_on_a_char_boundary_with_marker() -> Result<(), StoreError> {
        // Multi-byte chars (3 bytes each) so a naive byte cut would split one.
        let text: String = "気".repeat(4_000);
        let event = message(&text);
        let bounded = bound_event(&event, 512)?;
        let ActivityEventKind::Message {
            text: bounded_text, ..
        } = &bounded.kind
        else {
            return Err(StoreError::Backend("expected a Message kind".to_owned()));
        };
        assert!(
            bounded_text.contains("…[truncated"),
            "the marker names the truncation: {bounded_text}"
        );
        assert!(
            bounded_text.contains("observability.max_event_bytes"),
            "the marker names the operator knob"
        );
        // The kept prefix is whole chars: the string is valid UTF-8 by
        // construction (or `format!` would have panicked), so assert the cut
        // landed between the repeated 3-byte chars.
        let kept = bounded_text
            .split(" …[truncated")
            .next()
            .unwrap_or_default();
        assert_eq!(kept.len() % 3, 0, "the cut lands on a char boundary");
        assert!(serialized_len(&bounded)? <= 512);
        Ok(())
    }

    #[test]
    fn tool_result_output_is_replaced_with_truncation_stub() -> Result<(), StoreError> {
        let event = envelope(ActivityEventKind::ToolResult {
            call_id: "call-1".to_owned(),
            output: serde_json::json!({ "blob": "x".repeat(10_000) }),
            is_error: false,
        });
        let bounded = bound_event(&event, 512)?;
        let ActivityEventKind::ToolResult {
            call_id,
            output,
            is_error,
        } = &bounded.kind
        else {
            return Err(StoreError::Backend("expected a ToolResult kind".to_owned()));
        };
        assert_eq!(call_id, "call-1");
        assert!(!is_error);
        assert_eq!(output["truncated"], serde_json::json!(true));
        assert_eq!(
            output["reason"],
            serde_json::json!("observability.max_event_bytes")
        );
        assert!(output["original_bytes"].as_u64().unwrap_or(0) > 10_000);
        assert!(serialized_len(&bounded)? <= 512);
        Ok(())
    }

    #[test]
    fn raw_value_is_replaced_with_truncation_stub() -> Result<(), StoreError> {
        let event = envelope(ActivityEventKind::Raw {
            source: "unknown-harness".to_owned(),
            value: serde_json::json!({ "blob": "y".repeat(10_000) }),
        });
        let bounded = bound_event(&event, 512)?;
        let ActivityEventKind::Raw { source, value } = &bounded.kind else {
            return Err(StoreError::Backend("expected a Raw kind".to_owned()));
        };
        assert_eq!(source, "unknown-harness");
        assert_eq!(value["truncated"], serde_json::json!(true));
        assert!(serialized_len(&bounded)? <= 512);
        Ok(())
    }

    #[test]
    fn pathological_event_falls_back_to_note() -> Result<(), StoreError> {
        // The oversized payload is the agent_role — no kind reduction can help,
        // so the kind is replaced wholesale with the diagnostic note.
        let mut event = envelope(ActivityEventKind::Stop {
            reason: StopKind::EndTurn,
        });
        event.agent_role = "r".repeat(2_000);
        let bounded = bound_event(&event, 2_100)?;
        let ActivityEventKind::Progress {
            detail: ProgressDetail::Note { text },
        } = &bounded.kind
        else {
            return Err(StoreError::Backend("expected the note fallback".to_owned()));
        };
        assert!(text.contains("event truncated: stop"));
        assert!(text.contains("observability.max_event_bytes=2100"));
        Ok(())
    }

    /// Execution proof of the invariant: for every kind, an oversized event
    /// bounds to a record that re-serializes within a generous ceiling.
    #[test]
    fn every_kind_bounds_within_the_ceiling() -> Result<(), StoreError> {
        let big = "z".repeat(50_000);
        let ceiling = 4_096;
        let kinds = vec![
            ActivityEventKind::Message {
                role: MessageRole::User,
                text: big.clone(),
            },
            ActivityEventKind::ToolCall {
                tool: "read_file".to_owned(),
                call_id: "call-1".to_owned(),
                input: serde_json::json!({ "blob": big.clone() }),
            },
            ActivityEventKind::ToolResult {
                call_id: "call-1".to_owned(),
                output: serde_json::json!({ "blob": big.clone() }),
                is_error: true,
            },
            ActivityEventKind::Progress {
                detail: ProgressDetail::Note { text: big.clone() },
            },
            ActivityEventKind::Progress {
                detail: ProgressDetail::UsageEstimate {
                    input_tokens: Some(1),
                    output_tokens: None,
                },
            },
            ActivityEventKind::Stop {
                reason: StopKind::Error {
                    message: big.clone(),
                },
            },
            ActivityEventKind::Stop {
                reason: StopKind::Other {
                    reason: big.clone(),
                },
            },
            ActivityEventKind::Stop {
                reason: StopKind::EndTurn,
            },
            ActivityEventKind::Raw {
                source: "src".to_owned(),
                value: serde_json::json!([big.clone()]),
            },
        ];
        for kind in kinds {
            let bounded = bound_event(&envelope(kind), ceiling)?;
            assert!(
                serialized_len(&bounded)? <= ceiling,
                "every bounded kind re-serializes within the ceiling: {bounded:?}"
            );
        }
        Ok(())
    }
}
