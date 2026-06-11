//! Shared helpers for durable activity NIFs.

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
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_scheduled_started(recorded_at, activity_id, activity_type, input)
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
