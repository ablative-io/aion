//! Mapping libSQL and serde errors into `StoreError`.

use aion_store::StoreError;

/// Converts foreign backend and serialization errors into the closed `StoreError` taxonomy.
pub trait IntoStoreError {
    /// Convert this error into the `EventStore`-facing error surface.
    fn into_store_error(self) -> StoreError;
}

impl IntoStoreError for libsql::Error {
    fn into_store_error(self) -> StoreError {
        StoreError::Backend(self.to_string())
    }
}

impl IntoStoreError for serde_json::Error {
    fn into_store_error(self) -> StoreError {
        StoreError::Serialization(self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use aion_store::StoreError;

    use super::IntoStoreError;

    #[test]
    fn maps_libsql_error_to_backend() -> Result<(), Box<dyn std::error::Error>> {
        let error = libsql::Error::ConnectionFailed(String::from("database unavailable"));
        let mapped = error.into_store_error();

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
            .map_err(IntoStoreError::into_store_error)
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
