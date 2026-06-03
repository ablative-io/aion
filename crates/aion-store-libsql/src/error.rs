//! Mapping libSQL and serde errors into `StoreError`.

use aion_store::StoreError;

/// Map a libSQL driver error into the `StoreError::Backend` boundary variant.
#[must_use]
pub fn libsql_error(error: libsql::Error) -> StoreError {
    StoreError::Backend(error.to_string())
}

/// Map a JSON serialization or deserialization error into `StoreError::Serialization`.
#[must_use]
pub fn serde_json_error(error: serde_json::Error) -> StoreError {
    StoreError::Serialization(error.to_string())
}

#[cfg(test)]
mod tests {
    use aion_store::StoreError;

    use super::{libsql_error, serde_json_error};

    #[test]
    fn maps_libsql_error_to_backend() -> Result<(), Box<dyn std::error::Error>> {
        let error = libsql::Error::ConnectionFailed(String::from("database unavailable"));
        let mapped = libsql_error(error);

        match mapped {
            StoreError::Backend(message) => {
                assert!(message.contains("database unavailable"));
            }
            other => return Err(format!("expected backend error, got {other:?}").into()),
        }

        Ok(())
    }

    #[test]
    fn maps_serde_json_error_to_serialization() -> Result<(), Box<dyn std::error::Error>> {
        let error = serde_json::from_str::<serde_json::Value>("{")
            .map(|_| ())
            .map_err(serde_json_error)
            .err();

        match error {
            Some(StoreError::Serialization(message)) => {
                assert!(message.contains("EOF") || message.contains("object"));
            }
            Some(other) => {
                return Err(format!("expected serialization error, got {other:?}").into());
            }
            None => return Err("expected serde_json parsing to fail".into()),
        }

        Ok(())
    }
}
