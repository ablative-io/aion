//! Typed conversion helpers for `Payload` values.

use aion_core::{ContentType, Payload};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::ClientError;

/// JSON content type used by the typed helper surface.
pub const JSON_CONTENT_TYPE: ContentType = ContentType::Json;

/// Serializes a typed value into an `application/json` [`Payload`].
///
/// # Errors
///
/// Returns [`ClientError::InvalidArgument`] when `value` cannot be JSON-encoded.
pub fn to_payload<T>(value: &T) -> Result<Payload, ClientError>
where
    T: Serialize + ?Sized,
{
    let bytes = serde_json::to_vec(value).map_err(|source| {
        ClientError::invalid_argument(format!("value cannot be JSON-encoded: {source}"))
    })?;
    Ok(Payload::new(JSON_CONTENT_TYPE, bytes))
}

/// Deserializes a JSON [`Payload`] into a typed value.
///
/// Decode failures are mapped to [`ClientError::InvalidArgument`]: the payload is
/// present, but its bytes do not match the caller-requested typed shape.
///
/// # Errors
///
/// Returns [`ClientError::InvalidArgument`] when the payload is not valid JSON
/// for `T`.
pub fn from_payload<T>(payload: &Payload) -> Result<T, ClientError>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(payload.bytes()).map_err(|source| {
        ClientError::invalid_argument(format!(
            "payload bytes do not match the requested typed shape: {source}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::{from_payload, to_payload};
    use crate::error::ClientError;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct TypedPayload {
        label: String,
        count: u32,
    }

    #[test]
    fn typed_payload_round_trips_through_json_payload() -> Result<(), ClientError> {
        let value = TypedPayload {
            label: String::from("checkout"),
            count: 3,
        };

        let payload = to_payload(&value)?;
        let decoded: TypedPayload = from_payload(&payload)?;

        assert_eq!(decoded, value);
        Ok(())
    }
}
