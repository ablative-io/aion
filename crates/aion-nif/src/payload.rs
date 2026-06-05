//! `Payload` to `Term` bridge with serde blanket support via `Payload`.
//!
//! NIF declaration shims can route ordinary Rust structs through
//! [`from_term_via_payload`] and [`into_term_via_payload`]:
//!
//! ```ignore
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Deserialize, Serialize)]
//! struct RenderInput {
//!     template: String,
//!     count: u64,
//! }
//!
//! fn render(input: RenderInput) -> RenderInput {
//!     input
//! }
//! ```
//!
//! The AN-004 declaration macros call these helpers for JSON-shaped arguments
//! and return values so NIF authors do not write manual term or JSON handling.

use aion_core::Payload;
use beamr::{
    atom::Atom,
    native::ProcessContext,
    term::{Term, json},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{FromTerm, IntoTerm, TermError, raw};

/// Decode a beamr term into an aion-core JSON [`Payload`].
///
/// # Errors
///
/// Returns [`TermError`] when the context lacks an atom table, the term cannot
/// be represented as JSON by beamr, or the JSON value cannot be stored as a
/// payload.
pub fn payload_from_term(term: Term, ctx: &ProcessContext) -> Result<Payload, TermError> {
    let atom_table = ctx.atom_table().ok_or_else(|| TermError::AtomResolution {
        atom: "<json term>".to_owned(),
        reason: "atom table is unavailable".to_owned(),
    })?;
    let value = json::term_to_value(term, atom_table).map_err(|error| TermError::Conversion {
        context: "term to json value",
        message: error.to_string(),
    })?;

    Payload::from_json(&value).map_err(|error| TermError::Conversion {
        context: "json value to payload",
        message: error.to_string(),
    })
}

/// Encode an aion-core JSON [`Payload`] as a beamr term.
///
/// Payloads are handled explicitly by content type. Today `aion-core` exposes
/// only JSON payloads; future non-JSON payload content types should return a
/// [`TermError::Conversion`] here instead of being silently decoded as JSON.
///
/// # Errors
///
/// Returns [`TermError`] when the payload is not valid JSON for its content tag
/// or beamr cannot encode the JSON value as a term.
pub fn payload_into_term(payload: &Payload, ctx: &mut ProcessContext) -> Result<Term, TermError> {
    let value = payload.to_json().map_err(|error| TermError::Conversion {
        context: "payload to json value",
        message: error.to_string(),
    })?;

    value_into_term(&value, ctx)
}

/// Decode any serde value from a term through the JSON [`Payload`] bridge.
///
/// # Errors
///
/// Returns [`TermError`] when term-to-payload conversion fails or serde cannot
/// deserialize the JSON value as `T`.
pub fn from_term_via_payload<T>(term: Term, ctx: &ProcessContext) -> Result<T, TermError>
where
    T: DeserializeOwned,
{
    let payload = payload_from_term(term, ctx)?;
    let value = payload.to_json().map_err(|error| TermError::Conversion {
        context: "payload to json value",
        message: error.to_string(),
    })?;

    serde_json::from_value(value).map_err(|error| TermError::Conversion {
        context: "json value to serde type",
        message: error.to_string(),
    })
}

/// Encode any serde value as a term through the JSON [`Payload`] bridge.
///
/// # Errors
///
/// Returns [`TermError`] when serde cannot serialize `value`, payload creation
/// fails, or beamr cannot encode the JSON value as a term.
pub fn into_term_via_payload<T>(value: T, ctx: &mut ProcessContext) -> Result<Term, TermError>
where
    T: Serialize,
{
    let value = serde_json::to_value(value).map_err(|error| TermError::Conversion {
        context: "serde type to json value",
        message: error.to_string(),
    })?;
    let payload = Payload::from_json(&value).map_err(|error| TermError::Conversion {
        context: "json value to payload",
        message: error.to_string(),
    })?;

    payload_into_term(&payload, ctx)
}

fn value_into_term(value: &Value, ctx: &mut ProcessContext) -> Result<Term, TermError> {
    match value {
        Value::Null => Ok(ctx.allocate_term(Term::atom(Atom::NIL))),
        Value::Array(elements) => {
            let encoded = elements
                .iter()
                .map(|element| value_into_term(element, ctx))
                .collect::<Result<Vec<_>, _>>()?;
            raw::owned_list_term(&encoded)
        }
        Value::Object(object) => {
            let entries = {
                let atom_table = ctx.atom_table().ok_or_else(|| TermError::AtomResolution {
                    atom: "<json object key>".to_owned(),
                    reason: "atom table is unavailable".to_owned(),
                })?;
                object
                    .iter()
                    .map(|(key, value)| (Term::atom(atom_table.intern(key)), value))
                    .collect::<Vec<_>>()
            };
            let mut pairs = entries
                .into_iter()
                .map(|(key, value)| value_into_term(value, ctx).map(|value| (key, value)))
                .collect::<Result<Vec<_>, _>>()?;
            pairs.sort_by_key(|(key, _)| *key);

            let keys = pairs.iter().map(|(key, _)| *key).collect::<Vec<_>>();
            let values = pairs.iter().map(|(_, value)| *value).collect::<Vec<_>>();
            raw::owned_map_term(&keys, &values)
        }
        scalar => json::value_to_term(scalar, ctx).map_err(|error| TermError::Conversion {
            context: "json value to term",
            message: error.to_string(),
        }),
    }
}

impl FromTerm for Payload {
    fn from_term(term: Term, ctx: &ProcessContext) -> Result<Self, TermError> {
        payload_from_term(term, ctx)
    }
}

impl IntoTerm for Payload {
    fn into_term(self, ctx: &mut ProcessContext) -> Result<Term, TermError> {
        payload_into_term(&self, ctx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use beamr::{
        atom::AtomTable,
        native::ProcessContext,
        term::{Term, json},
    };
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use super::{
        from_term_via_payload, into_term_via_payload, payload_from_term, payload_into_term,
    };
    use crate::TermError;

    fn context() -> ProcessContext {
        let mut ctx = ProcessContext::new();
        ctx.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
        ctx
    }

    fn json_values() -> Vec<Value> {
        vec![
            Value::Null,
            serde_json::json!(true),
            serde_json::json!(123.45),
            serde_json::json!("hello"),
            serde_json::json!([null, false, 7, "item"]),
            serde_json::json!({"nested": {"value": null}, "array": [true, false]}),
        ]
    }

    #[test]
    fn payload_values_round_trip_losslessly_through_terms() -> Result<(), TermError> {
        for value in json_values() {
            let mut ctx = context();
            let payload =
                aion_core::Payload::from_json(&value).map_err(|error| TermError::Conversion {
                    context: "test json value to payload",
                    message: error.to_string(),
                })?;

            let term = payload_into_term(&payload, &mut ctx)?;
            let decoded = payload_from_term(term, &ctx)?;
            let decoded_value = decoded.to_json().map_err(|error| TermError::Conversion {
                context: "test payload to json value",
                message: error.to_string(),
            })?;

            assert_eq!(decoded_value, value);
        }

        Ok(())
    }

    #[test]
    fn payload_null_uses_atom_nil_not_empty_list() -> Result<(), TermError> {
        let mut ctx = context();
        let payload =
            aion_core::Payload::from_json(&Value::Null).map_err(|error| TermError::Conversion {
                context: "test json null to payload",
                message: error.to_string(),
            })?;

        let term = payload_into_term(&payload, &mut ctx)?;
        let atom_table = ctx.atom_table().ok_or_else(|| TermError::AtomResolution {
            atom: "nil".to_owned(),
            reason: "atom table is unavailable".to_owned(),
        })?;

        assert!(term.is_atom());
        assert_eq!(
            json::term_to_value(term, atom_table).ok(),
            Some(Value::Null)
        );
        assert_eq!(
            json::term_to_value(Term::NIL, atom_table).ok(),
            Some(serde_json::json!([]))
        );

        Ok(())
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct ExampleStruct {
        name: String,
        count: u64,
        enabled: bool,
    }

    #[test]
    fn serde_struct_round_trips_via_payload_terms() -> Result<(), TermError> {
        let mut ctx = context();
        let original = ExampleStruct {
            name: "render".to_owned(),
            count: 7,
            enabled: true,
        };

        let term = into_term_via_payload(&original, &mut ctx)?;
        let decoded = from_term_via_payload::<ExampleStruct>(term, &ctx)?;

        assert_eq!(decoded, original);
        Ok(())
    }

    #[test]
    fn serde_mismatch_returns_typed_conversion_error_with_message() -> Result<(), TermError> {
        let mut ctx = context();
        let term =
            into_term_via_payload(serde_json::json!({"name": 5, "count": "many"}), &mut ctx)?;
        let error = from_term_via_payload::<ExampleStruct>(term, &ctx);

        let Err(TermError::Conversion { context, message }) = error else {
            return Err(TermError::Conversion {
                context: "test serde mismatch assertion",
                message: "expected serde conversion error".to_owned(),
            });
        };

        assert_eq!(context, "json value to serde type");
        assert!(message.contains("name") || message.contains("string"));
        Ok(())
    }
}
