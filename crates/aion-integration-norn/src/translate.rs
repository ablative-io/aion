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
//! Plus [`parse_capabilities`], which gates the `initialize` result on the driven-mode protocol
//! version (`norn-driven/1`) and reads it into the neutral [`InterventionCapabilities`] the
//! server/console gate on, and [`run_result_to_output`], which interprets the `run/execute`
//! versioned stop envelope into the run's `output` value (or the honest error a non-completed
//! stop is).
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

/// Reads Norn's `initialize` result into the neutral [`InterventionCapabilities`], gating on the
/// driven-mode protocol version first.
///
/// The result must advertise `protocol: "norn-driven/1"` (which replaced the old
/// `protocolVersion: "2.0"`); a missing or different value is a [`HarnessError::Protocol`] naming
/// the expected and received values — the honest "your norn binary is stale" signal, surfaced
/// before any run is issued.
///
/// Norn advertises a `capabilities.interventions` array of neutral primitive labels
/// (`inject_message`, `cancel`, …). Unknown labels are ignored (forward-compat), and a missing
/// array yields the empty (observability-only) set — never an error, because an empty capability
/// advertisement is first-class.
///
/// # Errors
///
/// Returns [`HarnessError::Protocol`] when the `protocol` field is missing or is not exactly
/// [`protocol::PROTOCOL_VERSION`]; a present-but-wrong value (any JSON type) is rendered
/// verbatim in the message.
pub fn parse_capabilities(
    initialize_result: &Value,
) -> Result<InterventionCapabilities, HarnessError> {
    match initialize_result.get(protocol::PROTOCOL_KEY) {
        Some(received) if received.as_str() == Some(protocol::PROTOCOL_VERSION) => {}
        // A present-but-wrong value — including a non-string — is reported as the JSON actually
        // received (a string renders quoted), never misdescribed as a missing field.
        Some(received) => {
            return Err(HarnessError::protocol(format!(
                "norn driven-mode protocol mismatch: expected {:?}, received {received} — \
                 the norn binary is stale or incompatible with this adapter",
                protocol::PROTOCOL_VERSION,
            )));
        }
        None => {
            return Err(HarnessError::protocol(format!(
                "norn driven-mode protocol mismatch: expected {:?}, received no `{}` field — \
                 the norn binary is stale or incompatible with this adapter",
                protocol::PROTOCOL_VERSION,
                protocol::PROTOCOL_KEY,
            )));
        }
    }
    let labels = initialize_result
        .get(protocol::CAPABILITIES_KEY)
        .and_then(|caps| caps.get(protocol::INTERVENTIONS_KEY))
        .and_then(Value::as_array);
    let Some(labels) = labels else {
        return Ok(InterventionCapabilities::none());
    };
    let primitives = labels
        .iter()
        .filter_map(Value::as_str)
        .filter_map(capability_label_to_primitive);
    Ok(InterventionCapabilities::from_primitives(primitives))
}

/// Interprets a `run/execute` success result — the versioned stop envelope — into the run's
/// `output` value, or the honest error a non-completion is.
///
/// - `stop.reason == "completed"` → `Ok` with the `output` VALUE as-is (a JSON string for
///   schema-less runs, a JSON object when Norn ran with an output schema — never re-stringified).
/// - any other `stop.reason` (`schema_unreachable`, `max_iterations`, `timed_out`, `cancelled`,
///   `truncated`) → [`HarnessError::Harness`] whose message carries the whole `stop` object
///   verbatim, so the reason AND its per-variant detail fields (`elapsed_ms`, `attempts`,
///   `validation_errors`, `truncation`, …) survive into the error text the caller judges retry
///   on. When the envelope also carries a non-null `output` (the partial a `timed_out` /
///   `truncated` / `schema_unreachable` stop may hold), it rides in the same message, labelled
///   `partial output:` and bounded at [`PARTIAL_OUTPUT_MESSAGE_BYTES`], so a caller's
///   accept-the-partial policy stays reachable.
/// - a `completed` envelope with the `output` KEY ABSENT → [`HarnessError::Protocol`] — the
///   contract says a completed run always carries `output`. A present `"output": null` is a
///   legal null output and passes through.
/// - a result that is not the envelope shape (no `envelope_version` / `stop` / `stop.reason`) →
///   [`HarnessError::Protocol`] naming what was missing — no silent passthrough of unknown
///   shapes.
///
/// # Errors
///
/// Returns [`HarnessError::Protocol`] for a non-envelope result or a completed envelope without
/// an `output` field, and [`HarnessError::Harness`] for every non-`completed` stop.
pub fn run_result_to_output(result: &Value) -> Result<Value, HarnessError> {
    if result.get(protocol::ENVELOPE_VERSION_KEY).is_none() {
        return Err(HarnessError::protocol(format!(
            "run/execute result is not a norn stop envelope: missing `{}`",
            protocol::ENVELOPE_VERSION_KEY,
        )));
    }
    let stop = result.get(protocol::STOP_KEY).ok_or_else(|| {
        HarnessError::protocol(format!(
            "run/execute result is not a norn stop envelope: missing `{}`",
            protocol::STOP_KEY,
        ))
    })?;
    let reason = stop
        .get(protocol::STOP_REASON_KEY)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            HarnessError::protocol(format!(
                "run/execute result is not a norn stop envelope: `{}` carries no string `{}`",
                protocol::STOP_KEY,
                protocol::STOP_REASON_KEY,
            ))
        })?;
    if reason != protocol::STOP_REASON_COMPLETED {
        // The whole stop object goes into the message so the reason and its per-variant detail
        // fields (timed_out{elapsed_ms,iterations}, schema_unreachable{attempts,
        // validation_errors}, truncated{truncation,iterations}) survive verbatim for the caller
        // to judge retry on. A partial `output` rides along, clearly labelled, so accepting the
        // partial stays a reachable caller policy.
        let mut message = format!("run stopped without completing: stop: {stop}");
        if let Some(partial) = result
            .get(protocol::OUTPUT_KEY)
            .filter(|output| !output.is_null())
        {
            message.push_str("; partial output: ");
            message.push_str(&bounded_for_message(&partial.to_string()));
        }
        return Err(HarnessError::harness(message));
    }
    // The contract says a completed envelope ALWAYS carries `output`; a frame without the key is
    // off-contract. A present `"output": null` is a legal null output and passes through.
    match result.get(protocol::OUTPUT_KEY) {
        Some(output) => Ok(output.clone()),
        None => Err(HarnessError::protocol(
            "completed envelope carried no output field",
        )),
    }
}

/// The most of a non-completed run's partial `output` that rides in the harness-error message.
///
/// The bound is presentational truncation of an ERROR MESSAGE, not data loss — the run has
/// already failed; it only keeps an enormous partial from bloating the persisted failure text.
const PARTIAL_OUTPUT_MESSAGE_BYTES: usize = 4000;

/// Bounds `rendered` to [`PARTIAL_OUTPUT_MESSAGE_BYTES`], appending an explicit truncation
/// marker when it was cut. The cut lands on a char boundary so the message stays valid UTF-8.
fn bounded_for_message(rendered: &str) -> String {
    if rendered.len() <= PARTIAL_OUTPUT_MESSAGE_BYTES {
        return rendered.to_owned();
    }
    let mut end = PARTIAL_OUTPUT_MESSAGE_BYTES;
    while !rendered.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}… [partial output truncated for this message: {} of {} bytes shown]",
        &rendered[..end],
        end,
        rendered.len()
    )
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

    // --- capabilities parse (behind the protocol-version gate) ---

    #[test]
    fn parse_capabilities_reads_norns_advertised_set() {
        // The exact shape Norn's `initialize` result emits.
        let init = json!({
            "protocol": "norn-driven/1",
            "capabilities": { "interventions": ["inject_message", "cancel"] }
        });
        let caps = parse_capabilities(&init).unwrap();
        assert!(caps.supports_primitive(InterventionPrimitive::InjectMessage));
        assert!(caps.supports_primitive(InterventionPrimitive::Cancel));
        assert!(!caps.supports_primitive(InterventionPrimitive::PauseResume));
    }

    #[test]
    fn parse_capabilities_missing_array_is_empty_set() {
        let init = json!({ "protocol": "norn-driven/1", "capabilities": {} });
        let caps = parse_capabilities(&init).unwrap();
        assert!(
            caps.is_empty(),
            "no interventions advertised = observability-only"
        );
    }

    #[test]
    fn parse_capabilities_ignores_unknown_labels() {
        let init = json!({
            "protocol": "norn-driven/1",
            "capabilities": { "interventions": ["inject_message", "teleport"] }
        });
        let caps = parse_capabilities(&init).unwrap();
        assert!(caps.supports_primitive(InterventionPrimitive::InjectMessage));
        // "teleport" is unknown and silently ignored, never fabricated into a primitive.
        assert_eq!(caps.supported.len(), 1);
    }

    // --- the protocol-version gate ---

    #[test]
    fn missing_protocol_field_is_a_protocol_error_naming_expected_and_received() {
        // The pre-norn-driven/1 shape: `protocolVersion: "2.0"` and no `protocol` field. This is
        // the "your norn binary is stale" signal.
        let init = json!({
            "protocolVersion": "2.0",
            "capabilities": { "interventions": ["inject_message", "cancel"] }
        });
        let error = parse_capabilities(&init).unwrap_err();
        assert!(
            matches!(error, HarnessError::Protocol { .. }),
            "a stale advertisement is a protocol error, got {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("norn-driven/1"),
            "the error names the expected version: {message}"
        );
        assert!(
            message.contains("no `protocol` field"),
            "the error names what was received: {message}"
        );
    }

    #[test]
    fn different_protocol_version_is_a_protocol_error_naming_expected_and_received() {
        let init = json!({
            "protocol": "norn-driven/2",
            "capabilities": { "interventions": ["inject_message"] }
        });
        let error = parse_capabilities(&init).unwrap_err();
        assert!(matches!(error, HarnessError::Protocol { .. }));
        let message = error.to_string();
        assert!(
            message.contains("norn-driven/1") && message.contains("norn-driven/2"),
            "the error names the expected AND received versions: {message}"
        );
    }

    #[test]
    fn non_string_protocol_field_is_reported_as_the_value_received() {
        // A present-but-non-string `protocol` (e.g. `1`) must be reported as what actually
        // arrived, never misdescribed as a missing field.
        let error = parse_capabilities(&json!({ "protocol": 1 })).unwrap_err();
        assert!(matches!(error, HarnessError::Protocol { .. }));
        let message = error.to_string();
        assert!(
            message.contains("received 1"),
            "the error renders the received JSON value: {message}"
        );
        assert!(
            !message.contains("no `protocol` field"),
            "a present field must not be reported as missing: {message}"
        );
    }

    // --- the run/execute stop envelope ---

    #[test]
    fn completed_envelope_yields_the_output_value_as_is() {
        let envelope = json!({
            "envelope_version": 1,
            "stop": { "reason": "completed" },
            "output": { "answer": 7 },
        });
        assert_eq!(
            run_result_to_output(&envelope).unwrap(),
            json!({ "answer": 7 }),
            "a structured output passes through as the value, never re-stringified"
        );
    }

    #[test]
    fn non_completed_stop_is_a_harness_error_carrying_the_stop_object_and_partial() {
        let envelope = json!({
            "envelope_version": 1,
            "stop": { "reason": "timed_out", "elapsed_ms": 30000, "iterations": 4 },
            "output": "a half-written summary",
        });
        let error = run_result_to_output(&envelope).unwrap_err();
        assert!(matches!(error, HarnessError::Harness { .. }));
        let message = error.to_string();
        for fragment in ["timed_out", "elapsed_ms", "30000", "iterations", "4"] {
            assert!(
                message.contains(fragment),
                "the stop detail survives verbatim ({fragment}): {message}"
            );
        }
        assert!(
            message.contains("partial output: \"a half-written summary\""),
            "the partial output rides the message, labelled: {message}"
        );
    }

    #[test]
    fn non_completed_stop_with_null_output_carries_no_partial_label() {
        // `max_iterations`/`cancelled` envelopes carry `output: null` — nothing to preserve, so
        // the message must not fabricate a partial.
        let envelope = json!({
            "envelope_version": 1,
            "stop": { "reason": "max_iterations" },
            "output": null,
        });
        let error = run_result_to_output(&envelope).unwrap_err();
        let message = error.to_string();
        assert!(
            !message.contains("partial output"),
            "a null output is not a partial: {message}"
        );
    }

    #[test]
    fn an_enormous_partial_output_is_bounded_with_an_explicit_marker() {
        // The bound is presentational truncation of the error MESSAGE, not data loss — the run
        // already failed. The marker makes the cut explicit.
        let huge = "x".repeat(10_000);
        let envelope = json!({
            "envelope_version": 1,
            "stop": { "reason": "truncated", "truncation": "max_tokens", "iterations": 2 },
            "output": huge,
        });
        let error = run_result_to_output(&envelope).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("partial output: "),
            "the partial still rides, bounded: {message:.120}"
        );
        assert!(
            message.contains("truncated for this message"),
            "the truncation marker is explicit"
        );
        assert!(
            message.len() < 5_000,
            "the message is bounded, got {} bytes",
            message.len()
        );
    }

    #[test]
    fn a_completed_envelope_without_the_output_key_is_a_protocol_error() {
        // The contract says completed ALWAYS carries `output`; an absent key is off-contract and
        // must never be silently coerced into a null payload.
        let envelope = json!({
            "envelope_version": 1,
            "stop": { "reason": "completed" },
        });
        let error = run_result_to_output(&envelope).unwrap_err();
        assert!(
            matches!(error, HarnessError::Protocol { .. }),
            "a completed envelope without output is a protocol error, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("completed envelope carried no output field"),
            "the error names the off-contract shape: {error}"
        );
    }

    #[test]
    fn a_completed_envelope_with_a_present_null_output_passes_null_through() {
        // A present `"output": null` is a legal null output, distinct from a missing key.
        let envelope = json!({
            "envelope_version": 1,
            "stop": { "reason": "completed" },
            "output": null,
        });
        assert_eq!(run_result_to_output(&envelope).unwrap(), Value::Null);
    }

    #[test]
    fn a_result_without_envelope_version_is_a_protocol_error_naming_it() {
        // The pre-envelope shape must never silently pass through.
        let error = run_result_to_output(&json!({ "result": "completed" })).unwrap_err();
        assert!(matches!(error, HarnessError::Protocol { .. }));
        assert!(error.to_string().contains("envelope_version"));
    }

    #[test]
    fn a_result_without_stop_is_a_protocol_error_naming_it() {
        let error =
            run_result_to_output(&json!({ "envelope_version": 1, "output": "x" })).unwrap_err();
        assert!(matches!(error, HarnessError::Protocol { .. }));
        assert!(error.to_string().contains("`stop`"));
    }

    #[test]
    fn a_stop_without_a_string_reason_is_a_protocol_error() {
        let envelope = json!({ "envelope_version": 1, "stop": { "reason": 3 } });
        let error = run_result_to_output(&envelope).unwrap_err();
        assert!(matches!(error, HarnessError::Protocol { .. }));
        assert!(error.to_string().contains("`reason`"));
    }
}
