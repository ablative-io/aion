//! Runtime-owned conversions between durable payloads and BEAM terms.

use aion_core::{ContentType, Payload};
use beamr::atom::AtomTable;
use beamr::term::Term;

use crate::EngineError;

pub(crate) fn payload_to_term(payload: Payload) -> Result<Term, EngineError> {
    match payload.content_type() {
        ContentType::Json => json_to_term(payload.to_json().map_err(runtime_error_from_display)?),
    }
}

fn json_to_term(value: serde_json::Value) -> Result<Term, EngineError> {
    match value {
        serde_json::Value::Null => Ok(Term::NIL),
        serde_json::Value::Bool(true) => Ok(Term::atom(beamr::atom::Atom::TRUE)),
        serde_json::Value::Bool(false) => Ok(Term::atom(beamr::atom::Atom::FALSE)),
        serde_json::Value::Number(number) => {
            let value = number.as_i64().ok_or_else(|| {
                runtime_error(format!(
                    "json number `{number}` cannot become a BEAM small integer"
                ))
            })?;
            Term::try_small_int(value).ok_or_else(|| {
                runtime_error(format!(
                    "json number `{number}` is outside BEAM small integer range"
                ))
            })
        }
        serde_json::Value::String(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => Ok(Term::NIL),
    }
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
    Err(runtime_error(format!(
        "activity result term {term:?} cannot become a payload"
    )))
}

fn runtime_error(reason: String) -> EngineError {
    EngineError::Runtime { reason }
}

fn runtime_error_from_display(reason: impl std::fmt::Display) -> EngineError {
    runtime_error(reason.to_string())
}
