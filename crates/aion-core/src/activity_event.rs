//! The `ActivityEvent` envelope for the agent-observability real-time channel.
//!
//! This module defines the *typed contract* for a live, durable, per-`(workflow, activity,
//! attempt)` transcript of what an agent harness is doing inside an activity: its messages,
//! tool calls, tool results, progress, stop reasons, and (ephemeral) token deltas. It is the
//! sibling of [`crate::cluster_event`] — a **non-replay real-time DTO** that crosses the
//! Rust -> TypeScript boundary via `ts-rs` into the ops-console generated bindings.
//!
//! The wire shapes live in `aion-core` (not `aion-server` / the SDK) for the same reason the
//! cluster events do: only this leaf crate depends on `ts-rs`, so this is the single place a
//! Rust type can cross into the ops console's generated union. The `aion-integrations` SDK
//! re-exports these neutral types; the worker-side per-harness adapter is the single point that
//! maps a harness's native events into this envelope.
//!
//! # Harness neutrality (LOCKED)
//!
//! Every type in this module is **harness-neutral**: it names no agent harness, no transport,
//! and no wire protocol. There is no `Norn`, no JSON-RPC, and no stdio concept here. A harness
//! is integrated by mapping its native events into these shapes in the worker-side adapter,
//! never by editing this module. [`ActivityEventKind::Raw`] is the passthrough fallback that
//! makes the harness-agnostic path possible (and forward-compatible when a harness emits a
//! shape the adapter does not yet classify).
//!
//! # Observability, never replay
//!
//! An `ActivityEvent` is an observability record. It is **never** part of the workflow replay
//! log: the replay-authoritative output of an activity is its single terminal result, not its
//! transcript. These types deliberately carry no behaviour and no engine coupling — they are
//! pure data.
//!
//! # `u64` precision across the TS boundary
//!
//! The ts-rs config exports every `u64` as TS `number` (`with_large_int("number")`), which
//! truncates above `2^53`. [`ActivityEvent::worker_seq`] and [`ActivityEvent::store_seq`] are
//! `u64`. This is the *same* accepted ceiling that already applies to [`crate::EventEnvelope::seq`]
//! and the cluster channel's sequence fields; the transcript sequence follows the established
//! project convention rather than a divergent string encoding.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::{ActivityId, WorkflowId};

/// The role a conversational message is attributed to.
///
/// Harness-neutral: the worker-side adapter maps a harness's native speaker attribution onto
/// these roles. `Tool` covers a tool/function participant turn where the harness models one.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "role")]
pub enum MessageRole {
    /// A turn attributed to the operator / end user.
    User,
    /// A turn produced by the agent (model output).
    Assistant,
    /// A system / instruction turn.
    System,
    /// A turn attributed to a tool or function participant.
    Tool,
}

/// A fine-grained progress signal within an activity attempt.
///
/// Harness-neutral projection of the incremental, non-terminal signals a harness can emit
/// (streaming text/thinking fragments, tool-call argument streaming, usage estimates). It is a
/// tagged union so a harness advertises only the progress shapes it actually produces; anything
/// unclassifiable falls through to [`ActivityEventKind::Raw`] instead.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq)]
#[serde(tag = "detail")]
pub enum ProgressDetail {
    /// A running estimate of resource usage for the attempt so far.
    UsageEstimate {
        /// Estimated input (prompt) tokens consumed so far, when the harness reports it.
        input_tokens: Option<u64>,
        /// Estimated output (completion) tokens produced so far, when the harness reports it.
        output_tokens: Option<u64>,
    },
    /// A free-form, human-readable progress note the adapter could not model more precisely.
    Note {
        /// The progress note text.
        text: String,
    },
}

/// Why an agent run reached a terminal boundary.
///
/// Harness-neutral projection of a harness's native stop/finish reason. `Other` carries the
/// harness's raw reason label for reasons this neutral set does not enumerate, so no stop reason
/// is ever silently dropped.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "stop")]
pub enum StopKind {
    /// The agent completed its turn normally (produced its result).
    EndTurn,
    /// The agent stopped to await a tool result before continuing.
    ToolUse,
    /// The run hit a configured resource limit (tokens / turns / time).
    LimitReached,
    /// The run was cancelled (e.g. by an intervention or shutdown).
    Cancelled,
    /// The run stopped because of an error.
    Error {
        /// Human-readable error description.
        message: String,
    },
    /// A stop reason this neutral set does not enumerate; carries the harness's raw label.
    Other {
        /// The harness's raw stop-reason label.
        reason: String,
    },
}

/// The payload of an [`ActivityEvent`] — the classified kind of transcript signal.
///
/// **Kinds are LOCKED:** `Message`, `ToolCall`, `ToolResult`, `Progress`, `Stop`, `Raw`, plus
/// `Delta` carried on the same channel but flagged ephemeral (forwarded to the WS, never
/// persisted). [`Self::Raw`] is the passthrough fallback — critical for the harness-agnostic
/// path and for forward-compat when a harness adds an event shape the adapter does not yet map.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind")]
pub enum ActivityEventKind {
    /// A complete conversational message (assistant text/thinking, an operator turn, etc.).
    Message {
        /// Who the message is attributed to.
        role: MessageRole,
        /// The message text.
        text: String,
    },
    /// The agent invoked a tool/function with structured input.
    ToolCall {
        /// The tool/function name.
        tool: String,
        /// Correlation id linking this call to its eventual [`Self::ToolResult`].
        call_id: String,
        /// The structured tool input.
        #[ts(type = "unknown")]
        input: serde_json::Value,
    },
    /// A tool/function returned a result for a prior [`Self::ToolCall`].
    ToolResult {
        /// Correlation id matching the originating [`Self::ToolCall`].
        call_id: String,
        /// The structured tool output.
        #[ts(type = "unknown")]
        output: serde_json::Value,
        /// Whether the tool reported an error result.
        is_error: bool,
    },
    /// A fine-grained, non-terminal progress signal.
    Progress {
        /// The progress detail.
        detail: ProgressDetail,
    },
    /// The agent run reached a terminal boundary.
    Stop {
        /// Why the run stopped.
        reason: StopKind,
    },
    /// Passthrough fallback for an unmapped or other-harness line.
    ///
    /// Carries the source label the adapter observed and the raw value verbatim, so nothing is
    /// ever silently dropped and the harness-agnostic path stays lossless.
    Raw {
        /// A label identifying where the raw value came from (adapter-defined).
        source: String,
        /// The raw value, passed through verbatim.
        #[ts(type = "unknown")]
        value: serde_json::Value,
    },
    /// An ephemeral token delta — forwarded to the WS only, **never persisted**.
    ///
    /// Always carried with [`ActivityEvent::ephemeral`] set to `true`.
    Delta {
        /// The id of the message this fragment belongs to.
        message_id: String,
        /// The incremental text fragment.
        text_fragment: String,
    },
}

/// A live transcript event for one `(workflow, activity, attempt)` produced by an agent harness.
///
/// Streamed to the ops console in real time and persisted to a durable observability keyspace
/// (except [`Self::ephemeral`] events). It is **never** mixed into workflow replay history — the
/// activity's single terminal result is the replay-authoritative output, not this transcript.
///
/// # Ordering
///
/// [`Self::emitted_at`] and [`Self::worker_seq`] are best-effort producer-side ordering hints.
/// [`Self::store_seq`] is assigned by the server at durable-commit time and is `None` until the
/// event has been persisted — an unpersisted (e.g. ephemeral, or in-flight) event carries no
/// store sequence.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq)]
pub struct ActivityEvent {
    /// The workflow this activity belongs to.
    pub workflow_id: WorkflowId,
    /// The activity within the workflow.
    pub activity_id: ActivityId,
    /// The attempt number of the activity this event was produced during.
    pub attempt: u32,
    /// Sub-identity of the agent that produced this event — REQUIRED for multi-agent
    /// attribution (a single activity attempt may run several agents).
    pub agent_id: Uuid,
    /// The role/label of the producing agent (e.g. an orchestrator vs a sub-agent).
    pub agent_role: String,
    /// Producer-clock instant the event was emitted (best-effort ordering hint).
    pub emitted_at: DateTime<Utc>,
    /// Worker-local best-effort monotonic sequence.
    ///
    /// Exported to TypeScript as `number`; see the module docs for the accepted `2^53` ceiling.
    pub worker_seq: u64,
    /// Server-stamped monotonic sequence assigned at durable-commit time; `None` until the event
    /// is persisted (ephemeral events are never persisted and always carry `None`).
    ///
    /// Exported to TypeScript as `number`; see the module docs for the accepted `2^53` ceiling.
    pub store_seq: Option<u64>,
    /// When `true`, this event is WS-forward-only and is **never persisted** (token deltas).
    pub ephemeral: bool,
    /// The classified payload of this event.
    pub kind: ActivityEventKind,
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde::de::DeserializeOwned;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        ActivityEvent, ActivityEventKind, MessageRole, ProgressDetail, StopKind, WorkflowId,
    };
    use crate::ids::ActivityId;

    fn fixed_time() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default()
    }

    fn round_trip<T>(value: &T) -> Result<T, serde_json::Error>
    where
        T: DeserializeOwned + serde::Serialize,
    {
        let json = serde_json::to_string(value)?;
        serde_json::from_str::<T>(&json)
    }

    fn envelope(kind: ActivityEventKind, ephemeral: bool, store_seq: Option<u64>) -> ActivityEvent {
        ActivityEvent {
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(7),
            attempt: 2,
            agent_id: Uuid::nil(),
            agent_role: "orchestrator".to_owned(),
            emitted_at: fixed_time(),
            worker_seq: 42,
            store_seq,
            ephemeral,
            kind,
        }
    }

    #[test]
    fn envelope_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let event = envelope(
            ActivityEventKind::Message {
                role: MessageRole::Assistant,
                text: "hello".to_owned(),
            },
            false,
            Some(9),
        );
        let decoded = round_trip(&event)?;
        assert_eq!(event, decoded);
        Ok(())
    }

    #[test]
    fn every_event_kind_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let kinds = vec![
            ActivityEventKind::Message {
                role: MessageRole::User,
                text: "steer".to_owned(),
            },
            ActivityEventKind::ToolCall {
                tool: "read_file".to_owned(),
                call_id: "call-1".to_owned(),
                input: json!({ "path": "/tmp/x" }),
            },
            ActivityEventKind::ToolResult {
                call_id: "call-1".to_owned(),
                output: json!({ "bytes": 12 }),
                is_error: false,
            },
            ActivityEventKind::Progress {
                detail: ProgressDetail::UsageEstimate {
                    input_tokens: Some(100),
                    output_tokens: None,
                },
            },
            ActivityEventKind::Progress {
                detail: ProgressDetail::Note {
                    text: "thinking".to_owned(),
                },
            },
            ActivityEventKind::Stop {
                reason: StopKind::EndTurn,
            },
            ActivityEventKind::Stop {
                reason: StopKind::Error {
                    message: "boom".to_owned(),
                },
            },
            ActivityEventKind::Stop {
                reason: StopKind::Other {
                    reason: "custom".to_owned(),
                },
            },
            ActivityEventKind::Raw {
                source: "unknown-harness".to_owned(),
                value: json!({ "anything": [1, 2, 3] }),
            },
        ];
        for kind in kinds {
            let event = envelope(kind, false, None);
            let decoded = round_trip(&event)?;
            assert_eq!(event, decoded);
        }
        Ok(())
    }

    #[test]
    fn ephemeral_delta_round_trips_without_store_seq() -> Result<(), Box<dyn std::error::Error>> {
        let event = envelope(
            ActivityEventKind::Delta {
                message_id: "msg-1".to_owned(),
                text_fragment: "wor".to_owned(),
            },
            true,
            None,
        );
        let decoded = round_trip(&event)?;
        assert!(decoded.ephemeral);
        assert_eq!(decoded.store_seq, None);
        assert_eq!(event, decoded);
        Ok(())
    }
}
