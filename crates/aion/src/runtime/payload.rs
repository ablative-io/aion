//! Runtime-owned conversions between durable payloads and BEAM terms.

use aion_core::{ContentType, Payload};
use beamr::atom::AtomTable;
use beamr::term::Term;
use beamr::term::binary::Binary;
use beamr::term::boxed::Tuple;

use crate::EngineError;

/// A BEAM term plus host-owned heap words backing any boxed payload data.
///
/// beamr terms are tagged words; boxed binaries point into the heap slice that
/// created them. Keep these heap boxes alive while the spawned process may read
/// the argument term, then drop them through [`RuntimeHandle`](super::handle::RuntimeHandle)
/// lifecycle cleanup.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PayloadTerm {
    term: Term,
    heaps: Vec<Box<[u64]>>,
}

impl PayloadTerm {
    pub(crate) fn into_parts(self) -> (Term, Vec<Box<[u64]>>) {
        (self.term, self.heaps)
    }
}

pub(crate) fn payload_to_term(payload: &Payload) -> Result<PayloadTerm, EngineError> {
    match payload.content_type() {
        ContentType::Json => json_to_binary_term(payload.bytes()),
    }
}

fn json_to_binary_term(bytes: &[u8]) -> Result<PayloadTerm, EngineError> {
    use beamr::term::binary;
    let word_count = 2 + binary::packed_word_count(bytes.len());
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = binary::write_binary(&mut heap, bytes).ok_or_else(|| {
        runtime_error("could not allocate binary term for JSON payload".to_owned())
    })?;
    Ok(PayloadTerm {
        term,
        heaps: vec![heap],
    })
}

pub(crate) fn term_to_payload(term: Term, atoms: &AtomTable) -> Result<Payload, EngineError> {
    let value = term_to_json(term, atoms)?;
    Payload::from_json(&value).map_err(runtime_error_from_display)
}

fn term_to_json(term: Term, atoms: &AtomTable) -> Result<serde_json::Value, EngineError> {
    if term.is_nil() {
        return Ok(serde_json::Value::Null);
    }
    if let Some(value) = term.as_small_int() {
        return Ok(serde_json::Value::from(value));
    }
    if let Some(atom) = term.as_atom() {
        if atom == beamr::atom::Atom::TRUE {
            return Ok(serde_json::Value::Bool(true));
        }
        if atom == beamr::atom::Atom::FALSE {
            return Ok(serde_json::Value::Bool(false));
        }
        if let Some(name) = atoms.resolve(atom) {
            return Ok(serde_json::Value::String(name.to_owned()));
        }
    }
    if let Some(bin) = Binary::new(term) {
        return binary_to_json(bin.as_bytes());
    }
    if let Some(tuple) = Tuple::new(term) {
        return tuple_to_json(tuple, atoms);
    }
    Err(runtime_error(format!(
        "term {term:?} cannot become a payload"
    )))
}

fn binary_to_json(bytes: &[u8]) -> Result<serde_json::Value, EngineError> {
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| runtime_error("binary term is not valid UTF-8".to_owned()))?;
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => Ok(value),
        Err(_) => Ok(serde_json::Value::String(text)),
    }
}

fn tuple_to_json(tuple: Tuple, atoms: &AtomTable) -> Result<serde_json::Value, EngineError> {
    let elements: Result<Vec<serde_json::Value>, EngineError> = (0..tuple.arity())
        .map(|i| {
            let element = tuple
                .get(i)
                .ok_or_else(|| runtime_error(format!("tuple element {i} out of bounds")))?;
            term_to_json(element, atoms)
        })
        .collect();
    Ok(serde_json::Value::Array(elements?))
}

fn runtime_error(reason: String) -> EngineError {
    EngineError::Runtime { reason }
}

fn runtime_error_from_display(reason: impl std::fmt::Display) -> EngineError {
    runtime_error(reason.to_string())
}
