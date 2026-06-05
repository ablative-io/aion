//! `WorkerError` taxonomy.

/// Errors produced by worker configuration, protocol, transport, and payload boundaries.
#[derive(thiserror::Error, Debug)]
pub enum WorkerError {
    /// The worker could not connect to the configured endpoint.
    #[error("failed to connect to worker endpoint: {source}")]
    Connect {
        /// Transport connection failure reported by tonic.
        source: tonic::transport::Error,
    },

    /// The worker could not perform the initial protocol handshake.
    #[error("worker handshake failed: {source}")]
    Handshake {
        /// Handshake failure reported by the underlying transport.
        source: tonic::Status,
    },

    /// The worker could not register activity types.
    #[error("worker registration failed: {source}")]
    Registration {
        /// Registration failure source.
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// A payload or wire value could not be decoded.
    #[error("failed to decode worker payload: {source}")]
    Decode {
        /// Decode failure source.
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// A payload or wire value could not be encoded.
    #[error("failed to encode worker payload: {source}")]
    Encode {
        /// Encode failure source.
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// An established worker transport failed.
    #[error("worker transport failed: {source}")]
    Transport {
        /// Transport failure reported by tonic.
        source: tonic::Status,
    },
}

impl WorkerError {
    /// Creates a registration error from any source error.
    pub fn registration(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Registration {
            source: Box::new(source),
        }
    }

    /// Creates a decode error from any source error.
    pub fn decode(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Decode {
            source: Box::new(source),
        }
    }

    /// Creates an encode error from any source error.
    pub fn encode(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Encode {
            source: Box::new(source),
        }
    }
}

/// Error returned before serving when a requested activity type has no handler.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[error("activity type `{activity_type}` has no registered handler")]
pub struct MissingActivityHandler {
    /// Activity type requested for registration.
    pub activity_type: String,
}

#[cfg(test)]
mod tests {
    use super::{MissingActivityHandler, WorkerError};

    fn assert_send_sync_static<T: Send + Sync + 'static>() {}

    #[test]
    fn worker_error_is_send_sync_static() {
        assert_send_sync_static::<WorkerError>();
    }

    #[test]
    fn display_messages_name_failed_condition() {
        let error = WorkerError::registration(MissingActivityHandler {
            activity_type: String::from("charge-card"),
        });

        assert_eq!(
            error.to_string(),
            "worker registration failed: activity type `charge-card` has no registered handler"
        );
    }
}
