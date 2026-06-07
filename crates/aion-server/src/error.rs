//! `ServerError` taxonomy for server library modules.

use std::net::SocketAddr;

use aion::EngineError;
use aion_proto::WireError;
use aion_store::StoreError;
use thiserror::Error;

/// Server-library error taxonomy.
#[derive(Debug, Error)]
pub enum ServerError {
    /// Operator configuration could not be loaded or validated.
    #[error("configuration error: {message}")]
    Config {
        /// Redacted, operator-facing failure message.
        message: String,
    },

    /// A transport listener could not bind or start.
    #[error("{transport} transport failed at {address}: {message}")]
    TransportBind {
        /// Transport name.
        transport: &'static str,
        /// Configured listener address.
        address: SocketAddr,
        /// Redacted, operator-facing failure message.
        message: String,
    },

    /// Namespace validation or authorization failed.
    #[error("namespace error: {message}")]
    Namespace {
        /// Redacted namespace failure message.
        message: String,
    },

    /// Engine call failed.
    #[error("engine call failed: {source}")]
    EngineCall {
        /// Typed engine error returned by the embedded engine.
        #[from]
        source: EngineError,
    },

    /// Store backend call failed before an engine handle was available.
    #[error("store backend failed: {source}")]
    StoreBackend {
        /// Typed store error returned by the configured backend.
        #[from]
        source: StoreError,
    },

    /// Streaming failure.
    #[error("stream failure: {failure}")]
    Stream {
        /// Stream failure class.
        failure: StreamFailure,
    },

    /// A scheduled activity could not be pushed to a worker.
    #[error(
        "worker dispatch failed for namespace {namespace}, activity type {activity_type}: {reason}"
    )]
    WorkerDispatch {
        /// Namespace scoped before dispatch.
        namespace: String,
        /// Activity type requested by the engine.
        activity_type: String,
        /// Redacted dispatch failure reason.
        reason: String,
    },

    /// A lock was poisoned and the protected state cannot be trusted.
    #[error("{resource} lock was poisoned")]
    LockPoisoned {
        /// Protected resource name.
        resource: &'static str,
    },

    /// A failure already translated into the public wire taxonomy.
    #[error("wire error: {wire}")]
    Wire {
        /// Stable wire error.
        wire: WireError,
    },
}

/// Bounded-stream and connection failure classes.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum StreamFailure {
    /// Bounded per-connection buffer overflowed because the consumer lagged.
    #[error("consumer lagged behind bounded buffer")]
    Lagged,
    /// Subscriber closed the connection.
    #[error("subscriber connection closed")]
    Closed,
    /// Upstream engine event stream ended unexpectedly.
    #[error("engine event stream closed")]
    UpstreamClosed,
}

impl From<WireError> for ServerError {
    fn from(wire: WireError) -> Self {
        Self::Wire { wire }
    }
}

impl ServerError {
    /// Convert a server error that crosses a transport boundary into the stable
    /// public wire taxonomy.
    #[must_use]
    pub fn to_wire_error(&self) -> WireError {
        match self {
            Self::Config { .. } | Self::TransportBind { .. } | Self::LockPoisoned { .. } => {
                WireError::backend("server backend failure")
            }
            Self::WorkerDispatch { .. } => WireError::backend("worker dispatch failed"),
            Self::Namespace { .. } => WireError::namespace_denied("namespace access denied"),
            Self::EngineCall { source } => wire_from_engine(source),
            Self::StoreBackend { source } => wire_from_store(source),
            Self::Stream { failure } => match failure {
                StreamFailure::Lagged => WireError::lagged("subscriber lagged behind"),
                StreamFailure::Closed | StreamFailure::UpstreamClosed => {
                    WireError::backend("event stream closed")
                }
            },
            Self::Wire { wire } => wire.clone(),
        }
    }

    /// Construct a namespace-denied error without embedding authorization logic.
    #[must_use]
    pub fn namespace_denied(message: impl Into<String>) -> Self {
        Self::Namespace {
            message: message.into(),
        }
    }

    /// Construct a lagged-stream error.
    #[must_use]
    pub const fn lagged_stream() -> Self {
        Self::Stream {
            failure: StreamFailure::Lagged,
        }
    }

    /// Construct a worker-dispatch error.
    #[must_use]
    pub fn worker_dispatch(
        namespace: impl Into<String>,
        activity_type: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::WorkerDispatch {
            namespace: namespace.into(),
            activity_type: activity_type.into(),
            reason: reason.into(),
        }
    }

    /// Construct a lock-poison error at the lock boundary.
    #[must_use]
    pub const fn lock_poisoned(resource: &'static str) -> Self {
        Self::LockPoisoned { resource }
    }
}

fn wire_from_engine(source: &EngineError) -> WireError {
    match source {
        EngineError::WorkflowNotFound { .. } => WireError::not_found("workflow not found"),
        EngineError::ScheduleNotFound { .. } => WireError::not_found("schedule not found"),
        EngineError::ShuttingDown => WireError::not_running("engine is shutting down"),
        EngineError::Store(store) => wire_from_store(store),
        EngineError::Durability(durability) => match durability {
            aion::durability::DurabilityError::Store(store) => wire_from_store(store),
            aion::durability::DurabilityError::NonDeterminism(_)
            | aion::durability::DurabilityError::HistoryShape { .. } => {
                WireError::backend("durability failure")
            }
        },
        EngineError::MissingStore
        | EngineError::Load { .. }
        | EngineError::Package(_)
        | EngineError::Schedule { .. }
        | EngineError::Runtime { .. }
        | EngineError::RegistryPoisoned
        | EngineError::NifRegistration { .. } => WireError::backend("engine backend failure"),
    }
}

fn wire_from_store(source: &StoreError) -> WireError {
    match source {
        StoreError::SequenceConflict { .. } => {
            WireError::sequence_conflict("durable sequence conflict")
        }
        StoreError::NotFound { .. } => WireError::not_found("workflow not found"),
        StoreError::Backend(_) | StoreError::Serialization(_) => {
            WireError::backend("store backend failure")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ServerError, StreamFailure};
    use aion_proto::WireErrorCode;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn server_error_is_send_sync() {
        assert_send_sync::<ServerError>();
    }

    #[test]
    fn lagged_stream_maps_to_wire_lagged() {
        let error = ServerError::Stream {
            failure: StreamFailure::Lagged,
        };

        assert_eq!(error.to_wire_error().code, WireErrorCode::Lagged);
    }
}
