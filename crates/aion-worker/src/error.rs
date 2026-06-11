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

    /// The server kept closing the worker stream cleanly until the cumulative
    /// session-drop budget ran out without any session proving healthy.
    ///
    /// A single clean close is a retryable drop (the worker redials through
    /// the budgeted backoff cycle); this error surfaces only when a
    /// persistent clean-close loop exhausts `reconnect.max_attempts`.
    #[error(
        "worker session drop budget exhausted: the server repeatedly closed the stream cleanly"
    )]
    CleanCloseExhausted,
}

impl WorkerError {
    /// Creates a registration error from any source error.
    pub fn registration(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Registration {
            source: Box::new(source),
        }
    }

    /// Returns the underlying gRPC status carried by this error, if any.
    ///
    /// Handshake and transport failures carry a [`tonic::Status`] directly;
    /// registration failures preserve the status as their boxed source when the
    /// server rejected the registration over the wire.
    #[must_use]
    pub fn grpc_status(&self) -> Option<&tonic::Status> {
        match self {
            Self::Handshake { source } | Self::Transport { source } => Some(source),
            Self::Registration { source } => source.downcast_ref::<tonic::Status>(),
            Self::Connect { .. }
            | Self::Decode { .. }
            | Self::Encode { .. }
            | Self::CleanCloseExhausted => None,
        }
    }

    /// Returns whether retrying connection or registration can ever succeed.
    ///
    /// `PermissionDenied` and `Unauthenticated` are deterministic server
    /// denials (ungranted namespace, rejected credentials): retrying them only
    /// burns the reconnect budget and delays the surfaced error. Every other
    /// failure (transport unavailability, decode faults, local validation) is
    /// treated as transient for the bounded backoff loop.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        !matches!(
            self.grpc_status().map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied | tonic::Code::Unauthenticated)
        )
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

    #[test]
    fn registration_error_exposes_boxed_grpc_status() {
        let error = WorkerError::registration(tonic::Status::permission_denied(
            "namespace `payments` is not granted to subject `worker-a`",
        ));

        let status = error.grpc_status();
        assert!(matches!(
            status.map(tonic::Status::code),
            Some(tonic::Code::PermissionDenied)
        ));
        assert_eq!(
            status.map(tonic::Status::message),
            Some("namespace `payments` is not granted to subject `worker-a`")
        );
    }

    #[test]
    fn permission_denied_and_unauthenticated_are_not_retryable() {
        let denied = WorkerError::Handshake {
            source: tonic::Status::permission_denied("namespace not granted"),
        };
        let unauthenticated = WorkerError::Transport {
            source: tonic::Status::unauthenticated("credentials rejected"),
        };
        let denied_registration =
            WorkerError::registration(tonic::Status::permission_denied("namespace not granted"));

        assert!(!denied.is_retryable());
        assert!(!unauthenticated.is_retryable());
        assert!(!denied_registration.is_retryable());
    }

    #[test]
    fn transient_and_non_grpc_failures_stay_retryable() {
        let unavailable = WorkerError::Transport {
            source: tonic::Status::unavailable("engine unreachable"),
        };
        let local_registration = WorkerError::registration(MissingActivityHandler {
            activity_type: String::from("charge-card"),
        });

        assert!(unavailable.is_retryable());
        assert!(local_registration.is_retryable());
        assert!(local_registration.grpc_status().is_none());
    }
}
