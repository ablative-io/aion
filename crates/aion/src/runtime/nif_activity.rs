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
/// precedence exactly once (NSTQ-4).
///
/// Precedence: the per-activity override (`config.task_queue`) wins; absent
/// that, the workflow-level default (`config.workflow_task_queue`); absent both,
/// the named [`aion_core::DEFAULT_TASK_QUEUE`]. Both selections cross the FFI
/// boundary unresolved inside the activity-dispatch `config` JSON (the SDK's
/// `activity_config`), so this is the single seam where the precedence is
/// decided for both the single-schedule and the fan-out paths.
///
/// A `config` that is not valid JSON, or whose selection fields are absent or
/// non-string, deterministically resolves to the named default — the SDK always
/// emits valid config, so a malformed string signals a defect without taking
/// down an otherwise-valid dispatch over routing metadata (mirroring
/// [`labels_from_config`]). An explicit JSON `null` (the SDK's encoding of "no
/// selection") is treated as absent.
pub(super) fn resolve_task_queue(config: &str) -> String {
    let value = match serde_json::from_str::<serde_json::Value>(config) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                %error,
                "activity dispatch config was not valid JSON; routing to the default task queue"
            );
            return String::from(aion_core::DEFAULT_TASK_QUEUE);
        }
    };
    let selection = |field: &str| {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    selection("task_queue")
        .or_else(|| selection("workflow_task_queue"))
        .unwrap_or_else(|| String::from(aion_core::DEFAULT_TASK_QUEUE))
}

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

pub(super) fn record_started(
    ctx: &mut ProcessContext,
    context: &NifContext,
    activity_id: ActivityId,
    activity_type: String,
    input: Payload,
    task_queue: String,
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_scheduled_started(
            recorded_at,
            activity_id,
            activity_type,
            input,
            task_queue,
        )
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
    use super::resolve_task_queue;

    /// Build the activity-dispatch config the SDK's `activity_config` emits,
    /// with the two task-queue selection fields set to the supplied JSON.
    fn config(task_queue: &str, workflow_task_queue: &str) -> String {
        format!(
            r#"{{"retry":null,"timeout_ms":null,"heartbeat_ms":null,"labels":{{}},"task_queue":{task_queue},"workflow_task_queue":{workflow_task_queue}}}"#
        )
    }

    #[test]
    fn activity_override_wins_over_workflow_default() {
        // The per-activity selection is highest precedence.
        assert_eq!(
            resolve_task_queue(&config(r#""claude""#, r#""gpu""#)),
            "claude"
        );
    }

    #[test]
    fn workflow_default_applies_when_activity_selects_none() {
        // No activity override (JSON null) falls back to the workflow default.
        assert_eq!(resolve_task_queue(&config("null", r#""gpu""#)), "gpu");
    }

    #[test]
    fn no_selection_resolves_to_the_named_default() {
        // Neither selection set: the named "default" task queue.
        assert_eq!(
            resolve_task_queue(&config("null", "null")),
            aion_core::DEFAULT_TASK_QUEUE
        );
    }

    #[test]
    fn absent_fields_resolve_to_the_named_default() {
        // A config predating the fields (labels-only) decodes as no selection.
        assert_eq!(
            resolve_task_queue(r#"{"labels":{}}"#),
            aion_core::DEFAULT_TASK_QUEUE
        );
    }

    #[test]
    fn malformed_config_resolves_to_the_named_default() {
        // Invalid JSON never takes down a dispatch over routing metadata.
        assert_eq!(
            resolve_task_queue("{not json"),
            aion_core::DEFAULT_TASK_QUEUE
        );
    }
}
