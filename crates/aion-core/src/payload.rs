//! Opaque serialized payloads carried through histories and errors.

use serde::{Deserialize, Serialize};

/// Type-erased user data with an explicit content type tag.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Payload {
    content_type: ContentType,
    bytes: Vec<u8>,
}

impl Payload {
    /// Creates an opaque payload from a content type and byte buffer.
    #[must_use]
    pub fn new(content_type: ContentType, bytes: Vec<u8>) -> Self {
        Self {
            content_type,
            bytes,
        }
    }

    /// Serializes a JSON value into a payload tagged as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON value cannot be serialized.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, PayloadError> {
        let bytes = serde_json::to_vec(value)?;
        Ok(Self::new(ContentType::Json, bytes))
    }

    /// Deserializes this payload as a JSON value.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload is not tagged as JSON or the bytes do not contain valid
    /// JSON.
    pub fn to_json(&self) -> Result<serde_json::Value, PayloadError> {
        match self.content_type {
            ContentType::Json => Ok(serde_json::from_slice(&self.bytes)?),
        }
    }

    /// Returns the payload content type tag.
    #[must_use]
    pub const fn content_type(&self) -> &ContentType {
        &self.content_type
    }

    /// Returns the opaque serialized bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Stable tag describing the encoding used for a payload's bytes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ContentType {
    /// A `serde_json::Value` serialized as UTF-8 JSON bytes.
    Json,
}

/// Errors produced when converting payloads to or from typed values.
#[derive(thiserror::Error, Debug)]
pub enum PayloadError {
    /// JSON serialization or deserialization failed.
    #[error("json payload conversion failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ContentType, Payload};

    #[test]
    fn json_values_round_trip_losslessly() -> Result<(), Box<dyn std::error::Error>> {
        let values = [
            serde_json::Value::Null,
            json!(true),
            json!(123.45),
            json!("hello"),
            json!([null, false, 7, "item"]),
            json!({"nested": {"value": 1}, "array": [true, false]}),
        ];

        for value in values {
            let payload = Payload::from_json(&value)?;
            assert_eq!(payload.content_type(), &ContentType::Json);
            assert_eq!(payload.to_json()?, value);
        }

        Ok(())
    }
}
