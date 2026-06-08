//! `ServerError` taxonomy for server library modules.

use std::borrow::Cow;
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
            Self::Namespace { message } => WireError::namespace_denied(message.clone()),
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

    /// Return true when this is an operator configuration failure.
    #[must_use]
    pub const fn is_config(&self) -> bool {
        matches!(self, Self::Config { .. })
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

/// Stable structured error metadata for tracing events.
#[derive(Clone)]
pub struct ErrorTraceFields<'a> {
    /// Outer error type recorded in the `error_type` tracing field.
    pub error_type: Cow<'a, str>,
    /// Optional inner store error type for `StoreError` records.
    pub store_error_type: Option<&'static str>,
    /// Human-readable reason safe for operator logs.
    pub reason: &'a dyn std::fmt::Display,
}

impl ServerError {
    /// Return stable typed fields for structured error logging.
    #[must_use]
    pub fn trace_fields(&self) -> ErrorTraceFields<'_> {
        match self {
            Self::Config { message } => ErrorTraceFields {
                error_type: Cow::Borrowed("Config"),
                store_error_type: None,
                reason: message,
            },
            Self::TransportBind { message, .. } => ErrorTraceFields {
                error_type: Cow::Borrowed("TransportBind"),
                store_error_type: None,
                reason: message,
            },
            Self::Namespace { message } => ErrorTraceFields {
                error_type: Cow::Borrowed("Namespace"),
                store_error_type: None,
                reason: message,
            },
            Self::EngineCall { source } => engine_trace_fields(source),
            Self::StoreBackend { source } => store_trace_fields(source),
            Self::Stream { failure } => ErrorTraceFields {
                error_type: Cow::Borrowed("Stream"),
                store_error_type: None,
                reason: failure,
            },
            Self::WorkerDispatch { reason, .. } => ErrorTraceFields {
                error_type: Cow::Borrowed("WorkerDispatch"),
                store_error_type: None,
                reason,
            },
            Self::LockPoisoned { resource } => ErrorTraceFields {
                error_type: Cow::Borrowed("LockPoisoned"),
                store_error_type: None,
                reason: resource,
            },
            Self::Wire { wire } => ErrorTraceFields {
                error_type: wire
                    .error_type
                    .as_deref()
                    .map_or_else(|| Cow::Borrowed(wire.code.as_str()), Cow::Borrowed),
                store_error_type: None,
                reason: wire,
            },
        }
    }
}

fn engine_trace_fields(source: &EngineError) -> ErrorTraceFields<'_> {
    match source {
        EngineError::WorkflowNotFound { .. } => simple_engine_fields("WorkflowNotFound", source),
        EngineError::ScheduleNotFound { .. } => simple_engine_fields("ScheduleNotFound", source),
        EngineError::ShuttingDown => simple_engine_fields("ShuttingDown", source),
        EngineError::Store(store) => store_trace_fields(store),
        EngineError::Durability(durability) => match durability {
            aion::durability::DurabilityError::Store(store) => store_trace_fields(store),
            aion::durability::DurabilityError::NonDeterminism(_)
            | aion::durability::DurabilityError::HistoryShape { .. }
            | aion::durability::DurabilityError::SearchAttribute(_) => {
                simple_engine_fields("Durability", source)
            }
        },
        EngineError::MissingStore => simple_engine_fields("MissingStore", source),
        EngineError::Load { .. } => simple_engine_fields("Load", source),
        EngineError::Package(_) => simple_engine_fields("Package", source),
        EngineError::Schedule { .. } => simple_engine_fields("Schedule", source),
        EngineError::Runtime { .. } => simple_engine_fields("Runtime", source),
        EngineError::RegistryPoisoned => simple_engine_fields("RegistryPoisoned", source),
        EngineError::NifRegistration { .. } => simple_engine_fields("NifRegistration", source),
        EngineError::SignalRouter(_) => simple_engine_fields("SignalRouter", source),
    }
}

fn simple_engine_fields<'a>(
    error_type: &'static str,
    source: &'a EngineError,
) -> ErrorTraceFields<'a> {
    ErrorTraceFields {
        error_type: Cow::Borrowed(error_type),
        store_error_type: None,
        reason: source,
    }
}

fn store_trace_fields(source: &StoreError) -> ErrorTraceFields<'_> {
    ErrorTraceFields {
        error_type: Cow::Borrowed("StoreError"),
        store_error_type: Some(store_error_type(source)),
        reason: source,
    }
}

fn store_error_type(source: &StoreError) -> &'static str {
    match source {
        StoreError::SequenceConflict { .. } => "SequenceConflict",
        StoreError::NotFound { .. } => "NotFound",
        StoreError::Backend(_) => "Backend",
        StoreError::Serialization(_) => "Serialization",
    }
}

fn wire_from_engine(source: &EngineError) -> WireError {
    match source {
        EngineError::WorkflowNotFound { .. } => {
            WireError::not_found_with_type("WorkflowNotFound", source.to_string())
        }
        EngineError::ScheduleNotFound { .. } => {
            WireError::not_found_with_type("ScheduleNotFound", source.to_string())
        }
        EngineError::ShuttingDown => {
            WireError::not_running_with_type("ShuttingDown", source.to_string())
        }
        EngineError::Store(store) => wire_from_store(store),
        EngineError::Durability(durability) => match durability {
            aion::durability::DurabilityError::Store(store) => wire_from_store(store),
            aion::durability::DurabilityError::NonDeterminism(_)
            | aion::durability::DurabilityError::HistoryShape { .. }
            | aion::durability::DurabilityError::SearchAttribute(_) => {
                WireError::backend_with_type("Durability", source.to_string())
            }
        },
        EngineError::MissingStore => {
            WireError::backend_with_type("MissingStore", source.to_string())
        }
        EngineError::Load { .. } => WireError::backend_with_type("Load", source.to_string()),
        EngineError::Package(_) => WireError::backend_with_type("Package", source.to_string()),
        EngineError::Schedule { .. } => {
            WireError::backend_with_type("Schedule", source.to_string())
        }
        EngineError::Runtime { .. } => WireError::backend_with_type("Runtime", source.to_string()),
        EngineError::RegistryPoisoned => {
            WireError::backend_with_type("RegistryPoisoned", source.to_string())
        }
        EngineError::NifRegistration { .. } => {
            WireError::backend_with_type("NifRegistration", source.to_string())
        }
        EngineError::SignalRouter(_) => {
            WireError::backend_with_type("SignalRouter", source.to_string())
        }
    }
}

fn wire_from_store(source: &StoreError) -> WireError {
    match source {
        StoreError::SequenceConflict { .. } => WireError::new_with_type(
            aion_proto::WireErrorCode::SequenceConflict,
            "SequenceConflict",
            source.to_string(),
        ),
        StoreError::NotFound { .. } => {
            WireError::not_found_with_type("NotFound", source.to_string())
        }
        StoreError::Backend(_) => WireError::backend_with_type("Backend", source.to_string()),
        StoreError::Serialization(_) => {
            WireError::backend_with_type("Serialization", source.to_string())
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
