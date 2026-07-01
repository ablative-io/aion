//! The §3.4 translation point — the single place Norn's native event/command shapes are named.
//!
//! Two directions, both pure and independently testable:
//!
//! - **OUT:** [`notification_to_event`] maps a Norn `event/*` JSON-RPC notification (its `method`
//!   + `params`, as emitted by Norn's `agent_event_to_value` with `agent_id`/`agent_role` added)
//!   into a neutral [`ActivityEvent`]. Anything the adapter cannot classify falls through to
//!   [`ActivityEventKind::Raw`], so nothing is ever silently dropped.
//! - **IN:** [`intervention_to_request`] maps a neutral [`InterventionKind`] into the Norn
//!   `intervene/*` request (method + params). A primitive Norn does not advertise
//!   ([`InterventionKind::PauseResume`] / [`UpdateBudget`] / [`RespondToApproval`]) is rejected
//!   with [`HarnessError::CapabilityNotSupported`] and never sent.
//!
//! Plus [`parse_capabilities`], which reads Norn's `initialize` result into the neutral
//! [`InterventionCapabilities`] the server/console gate on.
//!
//! [`UpdateBudget`]: InterventionKind::UpdateBudget
//! [`RespondToApproval`]: InterventionKind::RespondToApproval

use aion_core::{
    ActivityEvent, ActivityEventKind, InterventionCapabilities, InterventionKind,
    InterventionPrimitive, MessageRole, ProgressDetail, StopKind,
};
use aion_integrations::HarnessError;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::protocol;

/// Identity carried on every event a session produces, stamped from the run spec + handshake.
///
/// The `(workflow, activity, attempt)` key and the per-notification `agent_id`/`agent_role` (read
/// from the notification params) together fill an [`ActivityEvent`]'s identity fields; the
/// producer-side ordering fields are filled by the session (a best-effort local sequence).
#[derive(Clone, Debug)]
pub struct EventIdentity {
    /// The workflow this run belongs to.
    pub workflow_id: aion_core::WorkflowId,
    /// The activity within the workflow.
    pub activity_id: aion_core::ActivityId,
    /// The attempt number of the run.
    pub attempt: u32,
}

/// Reads Norn's `initialize` result into the neutral [`InterventionCapabilities`].
///
/// Norn advertises a `capabilities.interventions` array of neutral primitive labels
/// (`inject_message`, `cancel`, …). Unknown labels are ignored (forward-compat), and a missing
/// array yields the empty (observability-only) set — never an error, because an empty capability
/// advertisement is first-class.
#[must_use]
pub fn parse_capabilities(initialize_result: &Value) -> InterventionCapabilities {
    let labels = initialize_result
        .get(protocol::CAPABILITIES_KEY)
        .and_then(|caps| caps.get(protocol::INTERVENTIONS_KEY))
        .and_then(Value::as_array);
    let Some(labels) = labels else {
        return InterventionCapabilities::none();
    };
    let primitives = labels
        .iter()
        .filter_map(Value::as_str)
        .filter_map(capability_label_to_primitive);
    InterventionCapabilities::from_primitives(primitives)
}

/// Maps a neutral capability label Norn advertises onto its [`InterventionPrimitive`].
///
/// Returns `None` for a label this neutral set does not enumerate, so an unknown label is ignored
/// rather than fabricated into a primitive.
fn capability_label_to_primitive(label: &str) -> Option<InterventionPrimitive> {
    match label {
        protocol::CAPABILITY_INJECT_MESSAGE => Some(InterventionPrimitive::InjectMessage),
        protocol::CAPABILITY_CANCEL => Some(InterventionPrimitive::Cancel),
        protocol::CAPABILITY_PAUSE_RESUME => Some(InterventionPrimitive::PauseResume),
        protocol::CAPABILITY_UPDATE_BUDGET => Some(InterventionPrimitive::UpdateBudget),
        protocol::CAPABILITY_RESPOND_TO_APPROVAL => Some(InterventionPrimitive::RespondToApproval),
        _ => None,
    }
}

/// The Norn `intervene/*` request a neutral command maps onto: a method plus its params.
///
/// Only the two primitives Norn advertises produce a request; the other three are rejected before
/// this returns, so a request is never built for an unsupported primitive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NornRequest {
    /// The Norn `intervene/*` method name.
    pub method: &'static str,
    /// The request params.
    pub params: Value,
}

/// Maps a neutral [`InterventionKind`] into the Norn `intervene/*` request that carries it.
///
/// `InjectMessage` → `intervene/injectMessage` (with a `text` + neutral `priority`), `Cancel` →
/// `intervene/cancel` (with a `reason`). The three primitives Norn does not advertise
/// (`PauseResume`, `UpdateBudget`, `RespondToApproval`) are rejected with
/// [`HarnessError::CapabilityNotSupported`] and NEVER sent — the adapter does not fabricate a
/// Norn mechanism that does not exist.
///
/// # Errors
///
/// Returns [`HarnessError::CapabilityNotSupported`] for a primitive Norn does not support.
pub fn intervention_to_request(kind: &InterventionKind) -> Result<NornRequest, HarnessError> {
    match kind {
        InterventionKind::InjectMessage { text, priority } => {
            let priority_label = match priority {
                aion_core::InjectPriority::Interrupt => protocol::PRIORITY_INTERRUPT,
                aion_core::InjectPriority::Normal => protocol::PRIORITY_NORMAL,
            };
            Ok(NornRequest {
                method: protocol::METHOD_INTERVENE_INJECT,
                params: json!({
                    protocol::PARAM_TEXT: text,
                    protocol::PARAM_PRIORITY: priority_label,
                }),
            })
        }
        InterventionKind::Cancel { reason } => Ok(NornRequest {
            method: protocol::METHOD_INTERVENE_CANCEL,
            params: json!({ protocol::PARAM_REASON: reason }),
        }),
        InterventionKind::PauseResume { .. }
        | InterventionKind::UpdateBudget { .. }
        | InterventionKind::RespondToApproval { .. } => Err(
            HarnessError::capability_not_supported(primitive_label(kind.primitive())),
        ),
    }
}

/// A neutral, human-readable label for a primitive (used in the capability-gate rejection).
fn primitive_label(primitive: InterventionPrimitive) -> &'static str {
    match primitive {
        InterventionPrimitive::InjectMessage => protocol::CAPABILITY_INJECT_MESSAGE,
        InterventionPrimitive::Cancel => protocol::CAPABILITY_CANCEL,
        InterventionPrimitive::PauseResume => protocol::CAPABILITY_PAUSE_RESUME,
        InterventionPrimitive::UpdateBudget => protocol::CAPABILITY_UPDATE_BUDGET,
        InterventionPrimitive::RespondToApproval => protocol::CAPABILITY_RESPOND_TO_APPROVAL,
    }
}

/// Maps a Norn `event/*` notification (`method` + `params`) into a neutral [`ActivityEvent`].
///
/// The `params` are Norn's native per-event payload (from `agent_event_to_value`) with
/// `agent_id` / `agent_role` added. The `method` carries the coarse category; the params' `type`
/// field carries the fine-grained native label. Any shape the classifier does not recognise falls
/// through to [`ActivityEventKind::Raw`] carrying the method as its source, so nothing is dropped.
#[must_use]
pub fn notification_to_event(
    identity: &EventIdentity,
    worker_seq: u64,
    method: &str,
    params: &Value,
) -> ActivityEvent {
    let (agent_id, agent_role) = event_attribution(params);
    let (kind, ephemeral) = classify_event(method, params);
    ActivityEvent {
        workflow_id: identity.workflow_id.clone(),
        activity_id: identity.activity_id.clone(),
        attempt: identity.attempt,
        agent_id,
        agent_role,
        emitted_at: Utc::now(),
        worker_seq,
        store_seq: None,
        ephemeral,
        kind,
    }
}

/// Reads the `agent_id` (a UUID string) and `agent_role` from a notification's params.
///
/// A missing/unparseable `agent_id` falls back to the nil UUID and a missing `agent_role` to an
/// empty label — attribution is best-effort and never fails the event.
fn event_attribution(params: &Value) -> (Uuid, String) {
    let agent_id = params
        .get(protocol::EVENT_AGENT_ID)
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .unwrap_or(Uuid::nil());
    let agent_role = params
        .get(protocol::EVENT_AGENT_ROLE)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    (agent_id, agent_role)
}

/// Classifies a notification into an [`ActivityEventKind`] and whether it is ephemeral.
///
/// Dispatches on the coarse `event/*` method, then refines on the params' native `type` label.
/// The delta arm is the only ephemeral one (WS-forward only, never persisted).
fn classify_event(method: &str, params: &Value) -> (ActivityEventKind, bool) {
    let event_type = params.get(protocol::EVENT_TYPE).and_then(Value::as_str);
    match method {
        "event/message" => (message_kind(event_type, params), false),
        "event/toolCall" => (tool_call_kind(params), false),
        "event/toolResult" => (tool_result_kind(params), false),
        "event/progress" => progress_kind(event_type, params),
        "event/stop" => (stop_kind(params), false),
        _ => (raw_kind(method, params), false),
    }
}

/// A required string field, or an empty string when absent — string fields are never fatal.
fn string_field(params: &Value, key: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Builds a [`ActivityEventKind::Message`] from an `event/message` notification.
///
/// Every Norn `event/message` is agent-produced (assistant text/thinking, or an inter-agent
/// message lifecycle), so the neutral role is always [`MessageRole::Assistant`]; an operator
/// injection is a distinct intervention, never an `event/message`. The message text is `text`
/// (assistant output) or the `content` of a message-lifecycle event when present.
fn message_kind(_event_type: Option<&str>, params: &Value) -> ActivityEventKind {
    let text = params
        .get("text")
        .or_else(|| params.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    ActivityEventKind::Message {
        role: MessageRole::Assistant,
        text,
    }
}

/// Builds a [`ActivityEventKind::ToolCall`] from an `event/toolCall` notification.
fn tool_call_kind(params: &Value) -> ActivityEventKind {
    ActivityEventKind::ToolCall {
        tool: string_field(params, "name"),
        call_id: string_field(params, "call_id"),
        input: params.get("arguments").cloned().unwrap_or(Value::Null),
    }
}

/// Builds a [`ActivityEventKind::ToolResult`] from an `event/toolResult` notification.
fn tool_result_kind(params: &Value) -> ActivityEventKind {
    ActivityEventKind::ToolResult {
        call_id: string_field(params, "tool_call_id"),
        output: params.get("output").cloned().unwrap_or(Value::Null),
        is_error: params
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// Builds a progress kind (and its ephemeral flag) from an `event/progress` notification.
///
/// Token/thinking deltas map to an ephemeral [`ActivityEventKind::Delta`]; a usage estimate maps
/// to a persisted [`ProgressDetail::UsageEstimate`]; anything else to a [`ProgressDetail::Note`].
fn progress_kind(event_type: Option<&str>, params: &Value) -> (ActivityEventKind, bool) {
    match event_type {
        Some("text_delta" | "thinking_delta") => {
            let delta = ActivityEventKind::Delta {
                message_id: string_field(params, "item_id"),
                text_fragment: string_field(params, "text"),
            };
            (delta, true)
        }
        Some("usage_estimate") => {
            let detail = ProgressDetail::UsageEstimate {
                input_tokens: params.get("input_tokens").and_then(Value::as_u64),
                output_tokens: params.get("output_tokens").and_then(Value::as_u64),
            };
            (ActivityEventKind::Progress { detail }, false)
        }
        _ => {
            let detail = ProgressDetail::Note {
                text: event_type.unwrap_or("progress").to_owned(),
            };
            (ActivityEventKind::Progress { detail }, false)
        }
    }
}

/// Builds a [`ActivityEventKind::Stop`] from an `event/stop` notification's `stop_reason`.
fn stop_kind(params: &Value) -> ActivityEventKind {
    let reason = match params.get("stop_reason").and_then(Value::as_str) {
        Some("end_turn") => StopKind::EndTurn,
        Some("tool_use") => StopKind::ToolUse,
        Some("max_tokens") => StopKind::LimitReached,
        Some(other) => StopKind::Other {
            reason: other.to_owned(),
        },
        None => StopKind::Other {
            reason: "unknown".to_owned(),
        },
    };
    ActivityEventKind::Stop { reason }
}

/// Builds a [`ActivityEventKind::Raw`] passthrough for an unclassified notification.
fn raw_kind(method: &str, params: &Value) -> ActivityEventKind {
    ActivityEventKind::Raw {
        source: method.to_owned(),
        value: params.clone(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use aion_core::{ActivityId, InjectPriority, WorkflowId};
    use serde_json::json;

    fn identity() -> EventIdentity {
        EventIdentity {
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(7),
            attempt: 3,
        }
    }

    /// The `event/*` notification params Norn emits (`agent_event_to_value` shape, with
    /// `agent_id` / `agent_role` added by its emitter).
    fn params(mut value: Value, agent_role: &str) -> Value {
        let obj = value.as_object_mut().unwrap();
        obj.insert(
            "agent_id".to_owned(),
            json!("11111111-1111-1111-1111-111111111111"),
        );
        obj.insert("agent_role".to_owned(), json!(agent_role));
        value
    }

    // --- OUT: event/* notification -> ActivityEvent, one per kind ---

    #[test]
    fn message_notification_maps_to_message_event() {
        let p = params(json!({ "type": "text", "text": "hello" }), "root");
        let event = notification_to_event(&identity(), 0, "event/message", &p);
        assert_eq!(event.attempt, 3);
        assert_eq!(event.agent_role, "root");
        assert_eq!(
            event.agent_id,
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
        );
        assert!(!event.ephemeral);
        match event.kind {
            ActivityEventKind::Message { role, text } => {
                assert_eq!(role, MessageRole::Assistant);
                assert_eq!(text, "hello");
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_notification_maps_to_tool_call_event() {
        let p = params(
            json!({
                "type": "tool_call",
                "call_id": "c1",
                "name": "read_file",
                "arguments": { "path": "/tmp/x" },
                "kind": "function",
            }),
            "root",
        );
        let event = notification_to_event(&identity(), 1, "event/toolCall", &p);
        match event.kind {
            ActivityEventKind::ToolCall {
                tool,
                call_id,
                input,
            } => {
                assert_eq!(tool, "read_file");
                assert_eq!(call_id, "c1");
                assert_eq!(input, json!({ "path": "/tmp/x" }));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_notification_maps_to_tool_result_event() {
        let p = params(
            json!({
                "type": "tool_result",
                "tool_call_id": "c1",
                "tool_name": "read_file",
                "output": { "bytes": 12 },
                "duration_ms": 3,
            }),
            "root",
        );
        let event = notification_to_event(&identity(), 2, "event/toolResult", &p);
        match event.kind {
            ActivityEventKind::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(output, json!({ "bytes": 12 }));
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn text_delta_notification_maps_to_ephemeral_delta() {
        let p = params(
            json!({ "type": "text_delta", "item_id": "m1", "text": "wor" }),
            "root",
        );
        let event = notification_to_event(&identity(), 3, "event/progress", &p);
        assert!(event.ephemeral, "a token delta is ephemeral");
        assert_eq!(event.store_seq, None);
        match event.kind {
            ActivityEventKind::Delta {
                message_id,
                text_fragment,
            } => {
                assert_eq!(message_id, "m1");
                assert_eq!(text_fragment, "wor");
            }
            other => panic!("expected Delta, got {other:?}"),
        }
    }

    #[test]
    fn usage_estimate_notification_maps_to_progress() {
        let p = params(
            json!({ "type": "usage_estimate", "input_tokens": 100 }),
            "root",
        );
        let event = notification_to_event(&identity(), 4, "event/progress", &p);
        assert!(!event.ephemeral);
        match event.kind {
            ActivityEventKind::Progress {
                detail: ProgressDetail::UsageEstimate { input_tokens, .. },
            } => assert_eq!(input_tokens, Some(100)),
            other => panic!("expected Progress::UsageEstimate, got {other:?}"),
        }
    }

    #[test]
    fn stop_notification_maps_stop_reasons() {
        for (label, expected) in [
            ("end_turn", StopKind::EndTurn),
            ("tool_use", StopKind::ToolUse),
            ("max_tokens", StopKind::LimitReached),
        ] {
            let p = params(json!({ "type": "done", "stop_reason": label }), "root");
            let event = notification_to_event(&identity(), 5, "event/stop", &p);
            match event.kind {
                ActivityEventKind::Stop { reason } => assert_eq!(reason, expected),
                other => panic!("expected Stop, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_stop_reason_falls_through_to_other() {
        let p = params(json!({ "type": "done", "stop_reason": "weird" }), "root");
        let event = notification_to_event(&identity(), 6, "event/stop", &p);
        match event.kind {
            ActivityEventKind::Stop {
                reason: StopKind::Other { reason },
            } => assert_eq!(reason, "weird"),
            other => panic!("expected Stop::Other, got {other:?}"),
        }
    }

    #[test]
    fn unclassifiable_notification_falls_through_to_raw() {
        let p = params(json!({ "type": "compaction", "item_type": "x" }), "root");
        let event = notification_to_event(&identity(), 7, "event/raw", &p);
        match event.kind {
            ActivityEventKind::Raw { source, value } => {
                assert_eq!(source, "event/raw");
                assert_eq!(value["type"], json!("compaction"));
            }
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn missing_agent_id_falls_back_to_nil_never_panics() {
        // A notification with no agent attribution still yields a well-formed event.
        let event = notification_to_event(
            &identity(),
            0,
            "event/message",
            &json!({ "type": "text", "text": "hi" }),
        );
        assert_eq!(event.agent_id, Uuid::nil());
        assert_eq!(event.agent_role, "");
    }

    // --- IN: neutral InterventionKind -> Norn intervene/* request ---

    #[test]
    fn inject_interrupt_maps_to_inject_message_request() {
        let req = intervention_to_request(&InterventionKind::InjectMessage {
            text: "use the other module".to_owned(),
            priority: InjectPriority::Interrupt,
        })
        .unwrap();
        assert_eq!(req.method, "intervene/injectMessage");
        assert_eq!(req.params["text"], json!("use the other module"));
        assert_eq!(req.params["priority"], json!("interrupt"));
    }

    #[test]
    fn inject_normal_maps_to_normal_priority() {
        let req = intervention_to_request(&InterventionKind::InjectMessage {
            text: "context".to_owned(),
            priority: InjectPriority::Normal,
        })
        .unwrap();
        assert_eq!(req.params["priority"], json!("normal"));
    }

    #[test]
    fn cancel_maps_to_cancel_request() {
        let req = intervention_to_request(&InterventionKind::Cancel {
            reason: "operator abort".to_owned(),
        })
        .unwrap();
        assert_eq!(req.method, "intervene/cancel");
        assert_eq!(req.params["reason"], json!("operator abort"));
    }

    #[test]
    fn unsupported_primitives_are_capability_not_supported_and_never_build_a_request() {
        for kind in [
            InterventionKind::PauseResume { paused: true },
            InterventionKind::UpdateBudget {
                max_tokens: Some(1),
                max_turns: None,
            },
            InterventionKind::RespondToApproval {
                call_id: "c".to_owned(),
                decision: aion_core::ApprovalDecision::Approve,
                note: None,
            },
        ] {
            let error = intervention_to_request(&kind).unwrap_err();
            assert!(
                matches!(error, HarnessError::CapabilityNotSupported { .. }),
                "an unsupported primitive must be capability-gated, got {error:?}"
            );
        }
    }

    // --- capabilities parse ---

    #[test]
    fn parse_capabilities_reads_norns_advertised_set() {
        // The exact shape Norn's `initialize_capabilities()` emits.
        let init = json!({
            "capabilities": { "interventions": ["inject_message", "cancel"] }
        });
        let caps = parse_capabilities(&init);
        assert!(caps.supports_primitive(InterventionPrimitive::InjectMessage));
        assert!(caps.supports_primitive(InterventionPrimitive::Cancel));
        assert!(!caps.supports_primitive(InterventionPrimitive::PauseResume));
    }

    #[test]
    fn parse_capabilities_missing_array_is_empty_set() {
        let caps = parse_capabilities(&json!({ "capabilities": {} }));
        assert!(
            caps.is_empty(),
            "no interventions advertised = observability-only"
        );
    }

    #[test]
    fn parse_capabilities_ignores_unknown_labels() {
        let init = json!({
            "capabilities": { "interventions": ["inject_message", "teleport"] }
        });
        let caps = parse_capabilities(&init);
        assert!(caps.supports_primitive(InterventionPrimitive::InjectMessage));
        // "teleport" is unknown and silently ignored, never fabricated into a primitive.
        assert_eq!(caps.supported.len(), 1);
    }
}
