//! Shared helpers for durable activity NIFs.

use std::collections::BTreeMap;
use std::sync::Arc;

use aion_core::{ActivityError, ActivityErrorKind, ActivityId, Payload};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary_ref::BinaryRef;
use chrono::Utc;
use tokio::runtime::Handle;

use crate::RuntimeHandle;
use crate::registry::Registry;
use crate::runtime::nif_context::{NifContext, NifContextError};
use crate::runtime::nif_state::EngineNifState;

#[derive(Clone)]
pub(super) struct RuntimeContext {
    pub(super) registry: Arc<Registry>,
    pub(super) runtime: Arc<RuntimeHandle>,
    pub(super) tokio_handle: Handle,
}

pub(crate) fn install_nif_runtime_context(
    state: &EngineNifState,
    registry: Arc<Registry>,
    runtime: Arc<RuntimeHandle>,
    tokio_handle: Handle,
) {
    let context = RuntimeContext {
        registry,
        runtime,
        tokio_handle,
    };
    match state.runtime_context.write() {
        Ok(mut slot) => *slot = Some(context),
        Err(poisoned) => *poisoned.into_inner() = Some(context),
    }
}

/// Extract the display labels a workflow attached to an activity from its
/// JSON-encoded dispatch config (the `labels` object the SDK's
/// `activity_config` emits).
///
/// Labels are optional display metadata, so a missing or non-string `labels`
/// entry yields an empty map rather than failing the dispatch. A `config`
/// that is not valid JSON at all is logged and treated as label-free — the
/// SDK always emits valid config, so that path signals a real defect without
/// taking down an otherwise-valid dispatch over display metadata.
pub(super) fn labels_from_config(config: &str) -> BTreeMap<String, String> {
    let value = match serde_json::from_str::<serde_json::Value>(config) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                %error,
                "activity dispatch config was not valid JSON; dispatching with no display labels"
            );
            return BTreeMap::new();
        }
    };
    value
        .get("labels")
        .and_then(serde_json::Value::as_object)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve the task queue one activity dispatch lands on, applying the locked
/// precedence exactly once (NSTQ-4, extended by #144).
///
/// Precedence, highest first:
///   1. the per-activity override (`config.task_queue`);
///   2. the workflow-level DECLARED default (`config.workflow_task_queue`) the
///      SDK author set;
///   3. `start_time_task_queue` — the queue the WORKFLOW WAS STARTED ON, read
///      from RECORDED HISTORY by the caller (#144);
///   4. the named [`aion_core::DEFAULT_TASK_QUEUE`], the absolute last resort
///      (a legacy history with no recorded start-time queue, or a start that
///      selected none anywhere).
///
/// Both config selections cross the FFI boundary unresolved inside the
/// activity-dispatch `config` JSON (the SDK's `activity_config`), so this is the
/// single seam where the precedence is decided for both the single-schedule and
/// the fan-out paths. The `start_time_task_queue` fallback is purely additive:
/// behaviour is unchanged whenever (1) or (2) supplies a queue — it only refines
/// the case that previously silently used the named default.
///
/// `start_time_task_queue` MUST be derived from recorded history (see
/// [`aion_core::start_time_task_queue`]), never from live or wall-clock state,
/// so recovery/replay re-resolve the SAME queue (this engine is
/// replay-deterministic).
///
/// A `config` that is not valid JSON, or whose selection fields are absent or
/// non-string, falls through to `start_time_task_queue` then the named default —
/// the SDK always emits valid config, so a malformed string signals a defect
/// without taking down an otherwise-valid dispatch over routing metadata
/// (mirroring [`labels_from_config`]). An explicit JSON `null` (the SDK's
/// encoding of "no selection") is treated as absent.
pub(super) fn resolve_task_queue(config: &str, start_time_task_queue: Option<&str>) -> String {
    let selection = config_task_queue_selection(config);
    selection
        .or_else(|| start_time_task_queue.map(str::to_owned))
        .unwrap_or_else(|| String::from(aion_core::DEFAULT_TASK_QUEUE))
}

/// The config-level task-queue selection: the per-activity override, else the
/// workflow-level declared default, else `None` (no selection in the config).
///
/// Splits the JSON decode + the (1)→(2) precedence out of [`resolve_task_queue`]
/// so the start-time-queue fallback (3) and the named default (4) read as a flat
/// `or_else` chain. A malformed or non-string config yields `None` (no
/// selection), matching [`labels_from_config`]'s "never fail a dispatch over
/// routing metadata" stance.
fn config_task_queue_selection(config: &str) -> Option<String> {
    let value = match serde_json::from_str::<serde_json::Value>(config) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                %error,
                "activity dispatch config was not valid JSON; falling back to the start-time or default task queue"
            );
            return None;
        }
    };
    let selection = |field: &str| {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    selection("task_queue").or_else(|| selection("workflow_task_queue"))
}

/// Resolve the OPTIONAL node affinity one activity dispatch pins to, decided
/// once at the schedule seam (NODE-4).
///
/// Unlike [`resolve_task_queue`], node affinity is optional and has no
/// workflow-level default: the precedence is simply the per-activity selection
/// (`config.node`) if set, else `None` (no affinity — dispatch to any worker in
/// the pool). This is the single seam where the optional node is decoded for
/// both the single-schedule and the fan-out paths; the selection crosses the
/// FFI boundary unresolved inside the activity-dispatch `config` JSON (the SDK's
/// `activity_config`).
///
/// A `config` that is not valid JSON, or whose `node` field is absent, JSON
/// `null` (the SDK's encoding of "no pin"), or non-string, deterministically
/// resolves to `None` — the SDK always emits valid config, so a malformed
/// string signals a defect without taking down an otherwise-valid dispatch over
/// routing metadata (mirroring [`labels_from_config`] and [`resolve_task_queue`]).
pub(super) fn resolve_node(config: &str) -> Option<String> {
    let value = match serde_json::from_str::<serde_json::Value>(config) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                %error,
                "activity dispatch config was not valid JSON; dispatching with no node affinity"
            );
            return None;
        }
    };
    value
        .get("node")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Read the OPTIONAL execution-tier selection from a dispatch `config` JSON
/// (the SDK's `activity_config` emits canonical `tier_to_string` values, or
/// JSON `null` for no selection).
///
/// This is a routing DEFENSE seam, not a router: the remote arity-3 wire and
/// the collect fan-out reject `"in_vm"` here (an in-VM dispatch must cross the
/// arity-4 wire that carries the runner thunk), and every other value —
/// absence, `null`, a remote tier, malformed JSON, a non-string — resolves to
/// `None`-or-passthrough exactly like [`resolve_node`]'s "never fail a
/// dispatch over routing metadata" stance. Remote tier VALUES are display
/// metadata to the engine today.
pub(super) fn config_tier(config: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(config).ok()?;
    value
        .get("tier")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The canonical in-VM tier wire value (`activity.tier_to_string(InVm)`).
pub(super) const IN_VM_TIER: &str = "in_vm";

pub(super) fn runtime_context(state: &EngineNifState) -> Result<RuntimeContext, NifContextError> {
    let guard = state
        .runtime_context
        .read()
        .map_err(|_| NifContextError::TermEncoding {
            reason: "nif runtime context lock is poisoned".to_owned(),
        })?;
    guard.clone().ok_or_else(|| NifContextError::TermEncoding {
        reason: "nif runtime context is not installed".to_owned(),
    })
}

/// Build `{Tag, <<bytes>>}` on the calling process heap.
///
/// Result terms are allocated through the [`ProcessContext`] allocators:
/// attached (normal-scheduler) calls get GC-traced process-heap terms, and
/// detached (dirty) calls get owned blocks the dirty-result bridge copies
/// onto the process heap. Nothing is parked in thread-locals — beamr's
/// moving GC never traces out-of-heap pointers, so a parked heap either
/// leaks for the scheduler thread's lifetime or dangles once cleared while
/// workflow code still references the term (N-6).
///
/// Allocation may collect on attached calls: decode every argument `Term`
/// before the first result allocation.
pub(super) fn tagged_result_term(
    ctx: &mut ProcessContext,
    tag: Atom,
    bytes: &[u8],
) -> Option<Term> {
    let value = ctx.alloc_binary(bytes).ok()?;
    ctx.alloc_tuple(&[Term::atom(tag), value]).ok()
}

pub(super) fn ok_result_term(ctx: &mut ProcessContext, bytes: &[u8]) -> Option<Term> {
    tagged_result_term(ctx, Atom::OK, bytes)
}

pub(super) fn error_result_term(ctx: &mut ProcessContext, message: &str) -> Option<Term> {
    tagged_result_term(ctx, Atom::ERROR, message.as_bytes())
}

pub(super) fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = BinaryRef::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

pub(super) fn json_payload(
    ctx: &mut ProcessContext,
    text: &str,
    phase: &str,
    label: &str,
) -> Result<Payload, Term> {
    let value = serde_json::from_str(text).map_err(|error| {
        error_result_term(
            ctx,
            &format!("{phase} {label}: invalid JSON payload: {error}"),
        )
        .unwrap_or(Term::NIL)
    })?;
    Payload::from_json(&value).map_err(|error| {
        error_result_term(ctx, &format!("{phase} {label}: {error}")).unwrap_or(Term::NIL)
    })
}

pub(super) fn activity_error(reason: String) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: reason,
        details: None,
    }
}

pub(super) fn context_error_term(ctx: &mut ProcessContext, error: &NifContextError) -> Term {
    error_result_term(ctx, &error.error_reason()).unwrap_or(Term::NIL)
}

/// The schedule-seam descriptor of one activity dispatch: everything the recorder stamps onto the
/// paired `ActivityScheduled` + `ActivityStarted` beyond the `activity_id`. Grouped so the schedule
/// seam stays a single cohesive parameter rather than a long positional list.
pub(crate) struct ScheduledActivity {
    /// Activity type the worker must execute.
    pub(super) activity_type: String,
    /// Opaque activity input payload.
    pub(super) input: Payload,
    /// Resolved task queue for this dispatch (NSTQ-4).
    pub(super) task_queue: String,
    /// Resolved OPTIONAL node affinity for this dispatch (NODE-4).
    pub(super) node: Option<String>,
    /// Genuine one-based delivery attempt this dispatch belongs to (NOI-0).
    pub(super) attempt: u32,
}

pub(super) fn record_started(
    ctx: &mut ProcessContext,
    context: &NifContext,
    activity_id: ActivityId,
    scheduled: ScheduledActivity,
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_scheduled_started(recorded_at, activity_id, scheduled)
        .map_err(|error| context_error_term(ctx, &error))
}

pub(super) fn correlation_id(ordinal: u64) -> String {
    ActivityId::from_sequence_position(ordinal).to_string()
}

pub(super) fn activity_id_from_correlation(
    ctx: &mut ProcessContext,
    correlation: &str,
) -> Result<ActivityId, Term> {
    let Some(raw) = correlation.strip_prefix("activity:") else {
        return Err(
            error_result_term(ctx, "await_activity_result: invalid correlation id")
                .unwrap_or(Term::NIL),
        );
    };
    let sequence = raw.parse::<u64>().map_err(|error| {
        error_result_term(
            ctx,
            &format!("await_activity_result: invalid correlation id sequence: {error}"),
        )
        .unwrap_or(Term::NIL)
    })?;
    Ok(ActivityId::from_sequence_position(sequence))
}

#[cfg(test)]
mod tests {
    use super::{config_tier, resolve_node, resolve_task_queue};

    /// Build the activity-dispatch config the SDK's `activity_config` emits,
    /// with the two task-queue selection fields set to the supplied JSON.
    fn config(task_queue: &str, workflow_task_queue: &str) -> String {
        format!(
            r#"{{"retry":null,"timeout_ms":null,"heartbeat_ms":null,"labels":{{}},"task_queue":{task_queue},"workflow_task_queue":{workflow_task_queue}}}"#
        )
    }

    /// Build the activity-dispatch config the SDK's `activity_config` emits,
    /// with the optional `node` affinity field set to the supplied JSON.
    fn config_with_node(node: &str) -> String {
        format!(
            r#"{{"retry":null,"timeout_ms":null,"heartbeat_ms":null,"labels":{{}},"task_queue":null,"workflow_task_queue":null,"node":{node}}}"#
        )
    }

    #[test]
    fn activity_override_wins_over_workflow_default() {
        // The per-activity selection is highest precedence.
        assert_eq!(
            resolve_task_queue(&config(r#""claude""#, r#""gpu""#), None),
            "claude"
        );
    }

    #[test]
    fn workflow_default_applies_when_activity_selects_none() {
        // No activity override (JSON null) falls back to the workflow default.
        assert_eq!(resolve_task_queue(&config("null", r#""gpu""#), None), "gpu");
    }

    #[test]
    fn no_selection_resolves_to_the_named_default() {
        // Neither selection set, no start-time queue: the named "default" queue.
        assert_eq!(
            resolve_task_queue(&config("null", "null"), None),
            aion_core::DEFAULT_TASK_QUEUE
        );
    }

    #[test]
    fn absent_fields_resolve_to_the_named_default() {
        // A config predating the fields (labels-only) decodes as no selection.
        assert_eq!(
            resolve_task_queue(r#"{"labels":{}}"#, None),
            aion_core::DEFAULT_TASK_QUEUE
        );
    }

    #[test]
    fn malformed_config_resolves_to_the_named_default() {
        // Invalid JSON never takes down a dispatch over routing metadata.
        assert_eq!(
            resolve_task_queue("{not json", None),
            aion_core::DEFAULT_TASK_QUEUE
        );
    }

    #[test]
    fn start_time_queue_applies_when_no_config_selection() {
        // #144: no activity override and no workflow declared default falls back
        // to the workflow's RECORDED start-time queue, NOT the named default.
        assert_eq!(
            resolve_task_queue(&config("null", "null"), Some("started-on")),
            "started-on"
        );
    }

    #[test]
    fn activity_override_wins_over_start_time_queue() {
        // #144 precedence: an explicit activity selector still wins over the
        // start-time queue.
        assert_eq!(
            resolve_task_queue(&config(r#""claude""#, "null"), Some("started-on")),
            "claude"
        );
    }

    #[test]
    fn workflow_default_wins_over_start_time_queue() {
        // #144 precedence: an SDK-declared workflow default still wins over the
        // start-time queue.
        assert_eq!(
            resolve_task_queue(&config("null", r#""gpu""#), Some("started-on")),
            "gpu"
        );
    }

    #[test]
    fn named_default_is_the_last_resort_below_start_time_queue() {
        // #144: only when there is no config selection AND no recorded
        // start-time queue does the named default apply (the legacy case).
        assert_eq!(
            resolve_task_queue(&config("null", "null"), None),
            aion_core::DEFAULT_TASK_QUEUE
        );
    }

    #[test]
    fn malformed_config_falls_through_to_start_time_queue() {
        // #144: a malformed config yields no selection, so the start-time queue
        // applies before the named default (never panics over routing metadata).
        assert_eq!(
            resolve_task_queue("{not json", Some("started-on")),
            "started-on"
        );
    }

    #[test]
    fn node_pin_resolves_to_the_selected_node() {
        // A config that pins node "box-7" resolves to Some("box-7").
        assert_eq!(
            resolve_node(&config_with_node(r#""box-7""#)),
            Some("box-7".to_owned())
        );
    }

    #[test]
    fn null_node_resolves_to_no_affinity() {
        // The SDK encodes "no pin" as JSON null: no affinity.
        assert_eq!(resolve_node(&config_with_node("null")), None);
    }

    #[test]
    fn absent_node_resolves_to_no_affinity() {
        // A config predating the field (labels-only) decodes as no affinity.
        assert_eq!(resolve_node(r#"{"labels":{}}"#), None);
    }

    #[test]
    fn malformed_config_resolves_to_no_node_affinity() {
        // Invalid JSON never takes down a dispatch over routing metadata.
        assert_eq!(resolve_node("{not json"), None);
    }

    #[test]
    fn tier_selection_is_read_from_config() {
        assert_eq!(
            config_tier(r#"{"labels":{},"tier":"in_vm"}"#),
            Some("in_vm".to_owned())
        );
        assert_eq!(
            config_tier(r#"{"labels":{},"tier":"remote_rust"}"#),
            Some("remote_rust".to_owned())
        );
    }

    #[test]
    fn absent_null_or_malformed_tier_resolves_to_no_selection() {
        // JSON null (the SDK's "no selection"), absence (a config predating the
        // field), a non-string, and malformed JSON all read as no selection.
        assert_eq!(config_tier(r#"{"labels":{},"tier":null}"#), None);
        assert_eq!(config_tier(r#"{"labels":{}}"#), None);
        assert_eq!(config_tier(r#"{"tier":7}"#), None);
        assert_eq!(config_tier("{not json"), None);
    }
}
