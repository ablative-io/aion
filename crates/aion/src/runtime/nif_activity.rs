//! Shared helpers for durable activity NIFs.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock, RwLock};

use aion_core::{ActivityError, ActivityErrorKind, ActivityId, Payload};
use beamr::atom::Atom;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed;
use chrono::Utc;
use tokio::runtime::Handle;

use crate::RuntimeHandle;
use crate::registry::Registry;
use crate::runtime::nif_context::{NifContext, NifContextError};

thread_local! {
    static ACTIVITY_NIF_HEAP: RefCell<Vec<Box<[u64]>>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
pub(super) struct RuntimeContext {
    pub(super) registry: Arc<Registry>,
    pub(super) runtime: Arc<RuntimeHandle>,
    pub(super) tokio_handle: Handle,
}

static RUNTIME_CONTEXT: OnceLock<RwLock<Option<RuntimeContext>>> = OnceLock::new();

pub(crate) fn install_nif_runtime_context(
    registry: Arc<Registry>,
    runtime: Arc<RuntimeHandle>,
    tokio_handle: Handle,
) {
    let context = RuntimeContext {
        registry,
        runtime,
        tokio_handle,
    };
    let cell = RUNTIME_CONTEXT.get_or_init(|| RwLock::new(None));
    if let Ok(mut slot) = cell.write() {
        *slot = Some(context);
    }
}

pub(super) fn runtime_context() -> Result<RuntimeContext, NifContextError> {
    let Some(cell) = RUNTIME_CONTEXT.get() else {
        return Err(NifContextError::TermEncoding {
            reason: "nif runtime context is not installed".to_owned(),
        });
    };
    let guard = cell.read().map_err(|_| NifContextError::TermEncoding {
        reason: "nif runtime context lock is poisoned".to_owned(),
    })?;
    guard.clone().ok_or_else(|| NifContextError::TermEncoding {
        reason: "nif runtime context is not installed".to_owned(),
    })
}

fn park_heap(heap: Box<[u64]>) {
    ACTIVITY_NIF_HEAP.with_borrow_mut(|parked| parked.push(heap));
}

pub(super) fn alloc_binary_term(bytes: &[u8]) -> Option<Term> {
    let word_count = 2 + binary::packed_word_count(bytes.len());
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = binary::write_binary(&mut heap, bytes)?;
    park_heap(heap);
    Some(term)
}

pub(super) fn alloc_tuple_term(elements: &[Term]) -> Option<Term> {
    let word_count = 1 + elements.len();
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = boxed::write_tuple(&mut heap, elements)?;
    park_heap(heap);
    Some(term)
}

pub(super) fn tagged_result_term(tag: Atom, bytes: &[u8]) -> Option<Term> {
    let value = alloc_binary_term(bytes)?;
    alloc_tuple_term(&[Term::atom(tag), value])
}

pub(super) fn ok_result_term(bytes: &[u8]) -> Option<Term> {
    tagged_result_term(Atom::OK, bytes)
}

pub(super) fn error_result_term(message: &str) -> Option<Term> {
    tagged_result_term(Atom::ERROR, message.as_bytes())
}

pub(super) fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = Binary::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

pub(super) fn json_payload(text: &str, phase: &str, label: &str) -> Result<Payload, Term> {
    let value = serde_json::from_str(text).map_err(|error| {
        error_result_term(&format!("{phase} {label}: invalid JSON payload: {error}"))
            .unwrap_or(Term::NIL)
    })?;
    Payload::from_json(&value).map_err(|error| {
        error_result_term(&format!("{phase} {label}: {error}")).unwrap_or(Term::NIL)
    })
}

pub(super) fn activity_error(reason: String) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: reason,
        details: None,
    }
}

pub(super) fn context_error_term(error: &NifContextError) -> Term {
    match error.to_error_term() {
        Ok(term) => term,
        Err(_) => Term::NIL,
    }
}

pub(super) fn record_started(
    context: &NifContext,
    activity_id: ActivityId,
    activity_type: String,
    input: Payload,
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_scheduled_started(recorded_at, activity_id, activity_type, input)
        .map_err(|error| context_error_term(&error))
}

pub(super) fn record_completed(
    context: &NifContext,
    activity_id: ActivityId,
    result: Payload,
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_completed(recorded_at, activity_id, result)
        .map_err(|error| context_error_term(&error))
}

pub(super) fn record_failed(
    context: &NifContext,
    activity_id: ActivityId,
    error: ActivityError,
) -> Result<(), Term> {
    let recorded_at = Utc::now();
    context
        .record_activity_failed(recorded_at, activity_id, error, 1)
        .map_err(|error| context_error_term(&error))
}

pub(super) fn correlation_id(ordinal: u64) -> String {
    ActivityId::from_sequence_position(ordinal).to_string()
}

pub(super) fn activity_id_from_correlation(correlation: &str) -> Result<ActivityId, Term> {
    let Some(raw) = correlation.strip_prefix("activity:") else {
        return Err(
            error_result_term("await_activity_result: invalid correlation id").unwrap_or(Term::NIL),
        );
    };
    let sequence = raw.parse::<u64>().map_err(|error| {
        error_result_term(&format!(
            "await_activity_result: invalid correlation id sequence: {error}"
        ))
        .unwrap_or(Term::NIL)
    })?;
    Ok(ActivityId::from_sequence_position(sequence))
}
