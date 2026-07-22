//! `ServerError` taxonomy for server library modules.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::path::PathBuf;

use aion::EngineError;
use aion_proto::WireError;
use aion_store::StoreError;
use thiserror::Error;

#[path = "error_engine.rs"]
mod engine;
#[path = "error_process_exit.rs"]
mod process_exit;

/// Server-library error taxonomy.
#[derive(Debug, Error)]
pub enum ServerError {
    /// Operator configuration could not be loaded or validated.
    #[error("configuration error: {message}")]
    Config {
        /// Redacted, operator-facing failure message.
        message: String,
    },

    /// A path-ambient store backend was configured beneath a renameable directory.
    #[error(
        "unsafe store.data_dir `{}`: ancestor `{}` is not owner-controlled: {reason}; \
         move store.data_dir beneath the private Aion home (`$AION_HOME`, default `~/.aion`) \
         and keep its ancestor chain owner-only",
        .data_root.display(),
        .component.display()
    )]
    UnsafeDataRootAncestor {
        /// Descriptor-resolved data root that the backend would use by pathname.
        data_root: PathBuf,
        /// First unsafe component in the resolved root's ancestor chain.
        component: PathBuf,
        /// Ownership, mode, or inspection failure that made the component unsafe.
        reason: String,
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

    /// A running transport task aborted: it panicked or was cancelled.
    #[error("{transport} transport task failed: {message}")]
    Transport {
        /// Transport name.
        transport: &'static str,
        /// Redacted, operator-facing failure message.
        message: String,
    },

    /// A termination-signal listener could not be installed or failed.
    #[error("{listener} listener failed: {message}")]
    SignalListener {
        /// Listener name (`SIGTERM`, `SIGINT`, or the portable fallback).
        listener: &'static str,
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

    /// The worker connection chosen for a dispatch was lost mid-flight: the
    /// connection was already gone at push time, or it closed before the worker
    /// sent its correlated push reply.
    ///
    /// This is DISTINCT from [`Self::WorkerDispatch`]: a `WorkerDispatch` covers a
    /// genuine reply timeout (the worker is alive but slow), a no-worker-available
    /// selection failure, or any other dispatch fault, all of which keep the
    /// outbox's normal exponential backoff. A `WorkerConnectionLost` instead means
    /// the chosen worker is gone (and has already been deregistered by liminal's
    /// `on_worker_unregistered`), so the row can be re-armed for IMMEDIATE re-claim
    /// to fail over to a live worker without waiting out the backoff. The outbox
    /// dispatcher keys its fast-failover decision on this variant.
    #[error("worker connection lost during dispatch on {channel}: {detail}")]
    WorkerConnectionLost {
        /// Row-derived dispatch channel for operator diagnostics.
        channel: String,
        /// Redacted, operator-facing description of how the connection was lost.
        detail: String,
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
            Self::Config { .. }
            | Self::UnsafeDataRootAncestor { .. }
            | Self::TransportBind { .. }
            | Self::Transport { .. }
            | Self::SignalListener { .. }
            | Self::LockPoisoned { .. } => WireError::backend("server backend failure"),
            Self::WorkerDispatch { .. } => WireError::backend("worker dispatch failed"),
            Self::WorkerConnectionLost { .. } => {
                WireError::backend("worker connection lost during dispatch")
            }
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
        matches!(
            self,
            Self::Config { .. } | Self::UnsafeDataRootAncestor { .. }
        )
    }

    /// Construct a namespace-denied error without embedding authorization logic.
    #[must_use]
    pub fn namespace_denied(message: impl Into<String>) -> Self {
        Self::Namespace {
            message: message.into(),
        }
    }

    /// Construct the loud, whole-registration rejection when a worker's advertised
    /// `node` violates a `Pinned{L}` namespace's placement (Control-Plane Phase 2,
    /// P2-I1). Names the offending namespace, the worker's advertised node (or
    /// "none"), and the required label set, so the operator sees exactly why the
    /// registration was refused. Carried on the namespace-denied wire code — a
    /// registration refused on isolation grounds is a namespace-authorization
    /// failure, not a transient dispatch error.
    #[must_use]
    pub fn placement_admission_denied(
        namespace: &str,
        worker_node: Option<&str>,
        required: &std::collections::BTreeSet<String>,
    ) -> Self {
        let node = worker_node.unwrap_or("none");
        let required = required
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        Self::namespace_denied(format!(
            "worker registration rejected: namespace {namespace} is Pinned to node label(s) \
             [{required}] but the worker advertises node {node}, which is not in the required set"
        ))
    }

    /// Construct a deploy-authorization denial carried on the dedicated
    /// `deploy_denied` wire code (deploy is not a namespace operation).
    #[must_use]
    pub fn deploy_denied(message: impl Into<String>) -> Self {
        Self::Wire {
            wire: WireError::deploy_denied(message),
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

    /// Construct a worker-connection-lost error for a dispatch whose chosen
    /// worker connection was gone at push time or closed before replying.
    #[must_use]
    pub fn worker_connection_lost(channel: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::WorkerConnectionLost {
            channel: channel.into(),
            detail: detail.into(),
        }
    }

    /// Return true when this is a lost-worker-connection dispatch failure.
    ///
    /// The outbox dispatcher keys its fast cross-node failover on this: a lost
    /// connection means the worker is gone (already deregistered), so the row is
    /// re-armed for immediate re-claim instead of waiting out the retry backoff.
    #[must_use]
    pub const fn is_worker_connection_lost(&self) -> bool {
        matches!(self, Self::WorkerConnectionLost { .. })
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
            Self::UnsafeDataRootAncestor { reason, .. } => ErrorTraceFields {
                error_type: Cow::Borrowed("UnsafeDataRootAncestor"),
                store_error_type: None,
                reason,
            },
            Self::TransportBind { message, .. } => ErrorTraceFields {
                error_type: Cow::Borrowed("TransportBind"),
                store_error_type: None,
                reason: message,
            },
            Self::Transport { message, .. } => ErrorTraceFields {
                error_type: Cow::Borrowed("Transport"),
                store_error_type: None,
                reason: message,
            },
            Self::SignalListener { message, .. } => ErrorTraceFields {
                error_type: Cow::Borrowed("SignalListener"),
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
            Self::WorkerConnectionLost { detail, .. } => ErrorTraceFields {
                error_type: Cow::Borrowed("WorkerConnectionLost"),
                store_error_type: None,
                reason: detail,
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
        EngineError::InvalidState { .. } => simple_engine_fields("InvalidState", source),
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
        EngineError::MissingVisibilityStore => {
            simple_engine_fields("MissingVisibilityStore", source)
        }
        EngineError::ConflictingEventPublisher => {
            simple_engine_fields("ConflictingEventPublisher", source)
        }
        EngineError::EventStreaming(_) => simple_engine_fields("EventStreaming", source),
        EngineError::Load { .. } => simple_engine_fields("Load", source),
        EngineError::UnknownVersion { .. } => simple_engine_fields("UnknownVersion", source),
        EngineError::VersionPinned { .. } => simple_engine_fields("VersionPinned", source),
        EngineError::RouteActive { .. } => simple_engine_fields("RouteActive", source),
        EngineError::ManifestMismatch { .. } => simple_engine_fields("ManifestMismatch", source),
        EngineError::Package(_) => simple_engine_fields("Package", source),
        EngineError::Schedule { .. } => simple_engine_fields("Schedule", source),
        EngineError::Runtime { .. } => simple_engine_fields("Runtime", source),
        EngineError::Gate3BifReplacementMissing { .. } => {
            simple_engine_fields("Gate3BifReplacementMissing", source)
        }
        EngineError::CleanupExecutorPoisoned => {
            simple_engine_fields("CleanupExecutorPoisoned", source)
        }
        EngineError::CleanupExecutorShutdownTimedOut { .. } => {
            simple_engine_fields("CleanupExecutorShutdownTimedOut", source)
        }
        EngineError::ProcessExitRegistryPoisoned => {
            simple_engine_fields("ProcessExitRegistryPoisoned", source)
        }
        EngineError::ProcessExitOwnershipPoisoned { .. } => {
            simple_engine_fields("ProcessExitOwnershipPoisoned", source)
        }
        EngineError::ProcessExitStatePoisoned { .. } => {
            process_exit::trace("ProcessExitStatePoisoned", source)
        }
        EngineError::ProcessExitSubscriptionUnavailable => {
            process_exit::trace("ProcessExitSubscriptionUnavailable", source)
        }
        EngineError::ProcessExitDrainerSpawn { .. } => {
            process_exit::trace("ProcessExitDrainerSpawn", source)
        }
        EngineError::ProcessExitDrainerPoisoned => {
            process_exit::trace("ProcessExitDrainerPoisoned", source)
        }
        EngineError::ProcessExitOutcomeMissingAfterEvent { .. } => {
            process_exit::trace("ProcessExitOutcomeMissingAfterEvent", source)
        }
        EngineError::ProcessExitEventStreamDisconnected => {
            process_exit::trace("ProcessExitEventStreamDisconnected", source)
        }
        EngineError::ProcessExitDrainerShutdownTimedOut { .. } => {
            process_exit::trace("ProcessExitDrainerShutdownTimedOut", source)
        }
        EngineError::ProcessExitDrainerPanicked => {
            process_exit::trace("ProcessExitDrainerPanicked", source)
        }
        EngineError::ProcessExitCallbackDispatcherPoisoned
        | EngineError::ProcessExitCallbackDispatcherUnavailable
        | EngineError::ProcessExitCallbackDispatcherShutdownTimedOut { .. } => {
            process_exit::callback_trace(source)
        }
        EngineError::ProcessExitAlreadyTerminal { .. } => {
            simple_engine_fields("ProcessExitAlreadyTerminal", source)
        }
        EngineError::ActivityDeliveryPoisoned { .. } => {
            simple_engine_fields("ActivityDeliveryPoisoned", source)
        }
        EngineError::RegistryPoisoned => simple_engine_fields("RegistryPoisoned", source),
        EngineError::CatalogPoisoned => simple_engine_fields("CatalogPoisoned", source),
        EngineError::NifRegistration { .. } => simple_engine_fields("NifRegistration", source),
        EngineError::SignalRouter(_) => simple_engine_fields("SignalRouter", source),
        EngineError::Query(query) => simple_engine_fields(engine::query_error_type(query), source),
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
        StoreError::NotOwner { .. } => "NotOwner",
        StoreError::Backend(_) => "Backend",
        StoreError::Serialization(_) => "Serialization",
    }
}

fn wire_from_engine(source: &EngineError) -> WireError {
    use EngineError as E;
    use engine::backend_wire as backend;

    match source {
        EngineError::WorkflowNotFound { .. } => {
            WireError::not_found_with_type("WorkflowNotFound", source.to_string())
        }
        // Reopen precondition failure (AD-012): the run is not a reopenable
        // terminal. Carried on the dedicated `invalid_state` wire code (gRPC
        // FailedPrecondition / HTTP 409), distinct from NotFound and Backend.
        EngineError::InvalidState { reason } => engine::invalid_state_wire(reason),
        EngineError::ScheduleNotFound { .. } => {
            WireError::not_found_with_type("ScheduleNotFound", source.to_string())
        }
        EngineError::ShuttingDown => {
            WireError::not_running_with_type("ShuttingDown", source.to_string())
        }
        EngineError::Store(store) => wire_from_store(store),
        EngineError::Durability(durability) => engine::durability_wire(durability, source),
        EngineError::MissingStore => engine::backend_wire("MissingStore", source),
        E::MissingVisibilityStore => backend("MissingVisibilityStore", source),
        E::ConflictingEventPublisher => backend("ConflictingEventPublisher", source),
        EngineError::EventStreaming(_) => engine::backend_wire("EventStreaming", source),
        EngineError::Load { .. } => WireError::backend_with_type("Load", source.to_string()),
        // Deploy-surface refusals (the §2.4 mapping table): unknown
        // `(type, version)` is not-found; route-active and pinned versions
        // are state conflicts carried by the dedicated `version_pinned`
        // code; a same-hash-different-manifest archive is invalid input.
        EngineError::UnknownVersion { .. } => {
            WireError::not_found_with_type("UnknownVersion", source.to_string())
        }
        EngineError::VersionPinned { .. } => {
            WireError::version_pinned(source.to_string()).with_error_type("VersionPinned")
        }
        EngineError::RouteActive { .. } => {
            WireError::version_pinned(source.to_string()).with_error_type("RouteActive")
        }
        EngineError::ManifestMismatch { .. } => {
            WireError::invalid_input(source.to_string()).with_error_type("ManifestMismatch")
        }
        EngineError::Package(_) => WireError::backend_with_type("Package", source.to_string()),
        EngineError::Schedule { .. } => {
            WireError::backend_with_type("Schedule", source.to_string())
        }
        EngineError::Runtime { .. } => WireError::backend_with_type("Runtime", source.to_string()),
        E::Gate3BifReplacementMissing { .. } => backend("Gate3BifReplacementMissing", source),
        EngineError::CleanupExecutorPoisoned => {
            WireError::backend_with_type("CleanupExecutorPoisoned", source.to_string())
        }
        EngineError::CleanupExecutorShutdownTimedOut { .. } => {
            WireError::backend_with_type("CleanupExecutorShutdownTimedOut", source.to_string())
        }
        EngineError::ProcessExitRegistryPoisoned => {
            WireError::backend_with_type("ProcessExitRegistryPoisoned", source.to_string())
        }
        EngineError::ProcessExitOwnershipPoisoned { .. } => {
            WireError::backend_with_type("ProcessExitOwnershipPoisoned", source.to_string())
        }
        EngineError::ProcessExitStatePoisoned { .. } => {
            process_exit::wire("ProcessExitStatePoisoned", source)
        }
        EngineError::ProcessExitSubscriptionUnavailable => {
            process_exit::wire("ProcessExitSubscriptionUnavailable", source)
        }
        EngineError::ProcessExitDrainerSpawn { .. } => {
            process_exit::wire("ProcessExitDrainerSpawn", source)
        }
        EngineError::ProcessExitDrainerPoisoned => {
            process_exit::wire("ProcessExitDrainerPoisoned", source)
        }
        EngineError::ProcessExitOutcomeMissingAfterEvent { .. } => {
            process_exit::wire("ProcessExitOutcomeMissingAfterEvent", source)
        }
        EngineError::ProcessExitEventStreamDisconnected => {
            process_exit::wire("ProcessExitEventStreamDisconnected", source)
        }
        EngineError::ProcessExitDrainerShutdownTimedOut { .. } => {
            process_exit::wire("ProcessExitDrainerShutdownTimedOut", source)
        }
        EngineError::ProcessExitDrainerPanicked => {
            process_exit::wire("ProcessExitDrainerPanicked", source)
        }
        EngineError::ProcessExitCallbackDispatcherPoisoned
        | EngineError::ProcessExitCallbackDispatcherUnavailable
        | EngineError::ProcessExitCallbackDispatcherShutdownTimedOut { .. } => {
            process_exit::callback_wire(source)
        }
        EngineError::ProcessExitAlreadyTerminal { .. } => {
            WireError::backend_with_type("ProcessExitAlreadyTerminal", source.to_string())
        }
        EngineError::ActivityDeliveryPoisoned { .. } => {
            WireError::backend_with_type("ActivityDeliveryPoisoned", source.to_string())
        }
        EngineError::CatalogPoisoned => {
            WireError::backend_with_type("CatalogPoisoned", source.to_string())
        }
        EngineError::RegistryPoisoned => {
            WireError::backend_with_type("RegistryPoisoned", source.to_string())
        }
        EngineError::NifRegistration { .. } => {
            WireError::backend_with_type("NifRegistration", source.to_string())
        }
        EngineError::SignalRouter(_) => {
            WireError::backend_with_type("SignalRouter", source.to_string())
        }
        EngineError::Query(query) => engine::query_wire(query, source),
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
        StoreError::NotOwner { .. } => {
            WireError::not_owner(source.to_string()).with_error_type("NotOwner")
        }
        StoreError::Backend(_) => WireError::backend_with_type("Backend", source.to_string()),
        StoreError::Serialization(_) => {
            WireError::backend_with_type("Serialization", source.to_string())
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
