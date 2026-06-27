//! `ClientError` taxonomy and transport/proto error mapping.
//!
//! Every variant carries an [`ErrorDetail`]: the server's human detail
//! message plus, when the wire carried one, the structured `error_type`
//! discriminator. Nothing the server sends is dropped on the client side —
//! callers branch on the variant, render `detail.message`, and may surface
//! `detail.error_type` for diagnostics.

use aion_proto::{ProtoWireError, WireError, WireErrorCode};
use prost::Message;
use tonic::Code;

/// Diagnostic payload carried by every [`ClientError`] variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDetail {
    /// Human-readable detail: the server's wire `message` when the error
    /// crossed the wire, a precise local description otherwise.
    pub message: String,
    /// Concrete typed server error variant (the wire `error_type` field),
    /// when the server exposed one.
    pub error_type: Option<String>,
}

impl ErrorDetail {
    /// Creates a detail with a message and no typed discriminator.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_type: None,
        }
    }

    /// Creates a detail carrying a typed `error_type` discriminator.
    #[must_use]
    pub fn with_type(message: impl Into<String>, error_type: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_type: Some(error_type.into()),
        }
    }
}

impl std::fmt::Display for ErrorDetail {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.error_type {
            Some(error_type) => write!(formatter, "{} [{error_type}]", self.message),
            None => formatter.write_str(&self.message),
        }
    }
}

impl From<String> for ErrorDetail {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for ErrorDetail {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

impl From<WireError> for ErrorDetail {
    fn from(error: WireError) -> Self {
        Self {
            message: error.message,
            error_type: error.error_type,
        }
    }
}

/// Branchable caller-side error taxonomy shared by every aion client SDK.
///
/// Display renders `<class>: <detail>` where `<class>` is the stable string
/// returned by [`ClientError::class`], aligned with the wire error codes
/// (`not_found`, `namespace_denied`, `invalid_input`, `backend`, ...).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The requested workflow or run does not exist.
    #[error("not_found: {detail}")]
    NotFound {
        /// Server-supplied detail message.
        detail: ErrorDetail,
    },
    /// A caller-supplied idempotency key conflicts with a different request.
    #[error("already_exists: {detail}")]
    AlreadyExists {
        /// Conflict detail message.
        detail: ErrorDetail,
    },
    /// The workflow query handler ran and reported an application failure.
    #[error("query_failed: {detail}")]
    QueryFailed {
        /// Handler failure detail reported by the workflow.
        detail: ErrorDetail,
    },
    /// The workflow query exceeded its deadline.
    #[error("query_timeout: {detail}")]
    QueryTimeout {
        /// Deadline detail (server window or local deadline).
        detail: ErrorDetail,
    },
    /// The requested workflow query name is not registered.
    #[error("unknown_query: {detail}")]
    UnknownQuery {
        /// Server-supplied detail naming the unknown query.
        detail: ErrorDetail,
    },
    /// The target workflow is terminal or otherwise not running.
    #[error("not_running: {detail}")]
    NotRunning {
        /// Server-supplied detail about the non-running target.
        detail: ErrorDetail,
    },
    /// The call or target workflow was cancelled.
    #[error("cancelled: {detail}")]
    Cancelled {
        /// Cancellation detail message.
        detail: ErrorDetail,
    },
    /// The server or network transport is unavailable.
    #[error("unavailable: {detail}")]
    Unavailable {
        /// Transport/connection failure detail.
        detail: ErrorDetail,
    },
    /// Authentication credentials were rejected.
    #[error("unauthenticated: {detail}")]
    Unauthenticated {
        /// Credential rejection detail.
        detail: ErrorDetail,
    },
    /// The caller's credential was accepted, but the caller has no grant for
    /// the requested namespace.
    ///
    /// This is exactly a namespace-grant failure. Workflow-level invisibility
    /// — the workflow does not exist, or is owned by another namespace — is
    /// reported as [`ClientError::NotFound`] so a cross-tenant probe is
    /// indistinguishable from a nonexistent workflow.
    ///
    /// Maps from the AW wire error code `namespace_denied` and gRPC
    /// `PERMISSION_DENIED`. Distinct from [`ClientError::Unauthenticated`]
    /// (credential rejected or unvalidatable) and from
    /// [`ClientError::InvalidArgument`] (malformed or invalid request). Not
    /// retryable until the caller's grants change.
    #[error("namespace_denied: {detail}")]
    NamespaceDenied {
        /// Server-supplied denial detail message.
        detail: ErrorDetail,
    },
    /// The request was malformed or targets an unsupported operation state.
    #[error("invalid_input: {detail}")]
    InvalidArgument {
        /// Precise description of what was invalid and how to fix it.
        detail: ErrorDetail,
    },
    /// The server reported an unexpected internal failure.
    #[error("backend: {detail}")]
    Server {
        /// Informational server detail.
        detail: ErrorDetail,
    },
}

macro_rules! detail_constructors {
    ($(($constructor:ident, $variant:ident, $doc:literal)),+ $(,)?) => {
        $(
            #[doc = $doc]
            #[must_use]
            pub fn $constructor(detail: impl Into<ErrorDetail>) -> Self {
                Self::$variant {
                    detail: detail.into(),
                }
            }
        )+
    };
}

impl ClientError {
    detail_constructors!(
        (not_found, NotFound, "Creates a not-found error."),
        (
            already_exists,
            AlreadyExists,
            "Creates an idempotency-conflict error."
        ),
        (
            query_failed,
            QueryFailed,
            "Creates a query-handler failure."
        ),
        (query_timeout, QueryTimeout, "Creates a query timeout."),
        (
            unknown_query,
            UnknownQuery,
            "Creates an unknown-query error."
        ),
        (not_running, NotRunning, "Creates a not-running error."),
        (cancelled, Cancelled, "Creates a cancellation error."),
        (
            unavailable,
            Unavailable,
            "Creates a transport-unavailable error."
        ),
        (
            unauthenticated,
            Unauthenticated,
            "Creates a credential-rejection error."
        ),
        (
            namespace_denied,
            NamespaceDenied,
            "Creates a namespace-grant denial."
        ),
        (
            invalid_argument,
            InvalidArgument,
            "Creates an [`ClientError::InvalidArgument`] carrying a precise message."
        ),
        (
            server,
            Server,
            "Creates an unexpected-server-failure error from a local conversion or server detail."
        ),
    );

    /// Stable taxonomy class string, aligned with the wire error codes.
    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::AlreadyExists { .. } => "already_exists",
            Self::QueryFailed { .. } => "query_failed",
            Self::QueryTimeout { .. } => "query_timeout",
            Self::UnknownQuery { .. } => "unknown_query",
            Self::NotRunning { .. } => "not_running",
            Self::Cancelled { .. } => "cancelled",
            Self::Unavailable { .. } => "unavailable",
            Self::Unauthenticated { .. } => "unauthenticated",
            Self::NamespaceDenied { .. } => "namespace_denied",
            Self::InvalidArgument { .. } => "invalid_input",
            Self::Server { .. } => "backend",
        }
    }

    /// The diagnostic detail carried by this error.
    #[must_use]
    pub const fn detail(&self) -> &ErrorDetail {
        match self {
            Self::NotFound { detail }
            | Self::AlreadyExists { detail }
            | Self::QueryFailed { detail }
            | Self::QueryTimeout { detail }
            | Self::UnknownQuery { detail }
            | Self::NotRunning { detail }
            | Self::Cancelled { detail }
            | Self::Unavailable { detail }
            | Self::Unauthenticated { detail }
            | Self::NamespaceDenied { detail }
            | Self::InvalidArgument { detail }
            | Self::Server { detail } => detail,
        }
    }

    /// Converts an AW wire error into the client SDK taxonomy, preserving the
    /// server's message and `error_type` in the carried [`ErrorDetail`].
    #[must_use]
    pub fn from_wire_error(error: WireError) -> Self {
        let code = error.code;
        let detail = ErrorDetail::from(error);
        match code {
            WireErrorCode::NotFound => Self::NotFound { detail },
            WireErrorCode::NamespaceDenied => Self::NamespaceDenied { detail },
            WireErrorCode::UnknownQuery => Self::UnknownQuery { detail },
            WireErrorCode::NotRunning => Self::NotRunning { detail },
            WireErrorCode::InvalidInput => Self::InvalidArgument { detail },
            // `sequence_conflict` is emitted solely for the server's internal
            // single-writer invariant violation (a double-writer bug). The
            // server has no idempotency-key feature, so this is never
            // AlreadyExists; it is an unexpected server failure.
            // `deploy_denied` / `version_pinned` belong to the operator
            // deploy surface, which the caller SDK contract deliberately
            // excludes (CLIENT-CONTRACT scope); they can never be returned
            // by a caller SDK operation, so they fall into the generic
            // server bucket rather than growing the caller taxonomy.
            WireErrorCode::SequenceConflict
            | WireErrorCode::Backend
            | WireErrorCode::DeployDenied
            | WireErrorCode::VersionPinned => Self::Server { detail },
            WireErrorCode::QueryFailed => Self::QueryFailed { detail },
            WireErrorCode::QueryTimeout => Self::QueryTimeout { detail },
            // `not_owner` (wrong-shard-owner fence) is a retryable routing
            // signal: the request reached a node that does not own the
            // workflow's shard. It is transient (re-resolve + retry), so it
            // joins the `Unavailable` bucket alongside the lagged-stream signal.
            WireErrorCode::Lagged | WireErrorCode::NotOwner => Self::Unavailable { detail },
        }
    }

    /// Converts a proto-encoded wire error into the client SDK taxonomy.
    #[must_use]
    pub fn from_proto_wire_error(error: ProtoWireError) -> Self {
        match WireError::try_from(error) {
            Ok(error) | Err(error) => Self::from_wire_error(error),
        }
    }

    /// Converts a tonic status into the client SDK taxonomy.
    ///
    /// The server encodes the full typed `WireError` (code, message,
    /// `error_type`) into the status details; when present it is
    /// authoritative. Without decodable details the gRPC code is mapped and
    /// the status message becomes the detail, so the server's human detail is
    /// never dropped.
    #[must_use]
    pub fn from_status(status: &tonic::Status) -> Self {
        if let Some(error) = decode_status_details(status) {
            return Self::from_proto_wire_error(error);
        }

        let detail = ErrorDetail::new(status.message());
        match status.code() {
            Code::NotFound => Self::NotFound { detail },
            Code::AlreadyExists => Self::AlreadyExists { detail },
            Code::DeadlineExceeded => Self::QueryTimeout { detail },
            Code::Cancelled => Self::Cancelled { detail },
            Code::Unavailable | Code::ResourceExhausted => Self::Unavailable { detail },
            Code::Unauthenticated => Self::Unauthenticated { detail },
            Code::PermissionDenied => Self::NamespaceDenied { detail },
            Code::InvalidArgument => Self::InvalidArgument { detail },
            // The server sends FAILED_PRECONDITION only for the `not_running`
            // wire code, so the bare gRPC code is still unambiguous.
            Code::FailedPrecondition => Self::NotRunning { detail },
            // ABORTED deliberately falls through to Server: the server sends
            // it only for `sequence_conflict`, an internal single-writer
            // invariant violation (a double-writer bug), never an
            // idempotency conflict — so it must not map to AlreadyExists.
            _ => Self::Server { detail },
        }
    }

    /// Converts a tonic transport failure into the client SDK taxonomy,
    /// preserving the full transport error chain as the detail message.
    #[must_use]
    pub fn from_transport_error(error: &tonic::transport::Error) -> Self {
        Self::Unavailable {
            detail: ErrorDetail::new(source_chain(error)),
        }
    }
}

/// Joins an error's Display with every `source()` cause, so transport errors
/// like tonic's bare "transport error" keep their underlying connect/DNS/TLS
/// detail.
fn source_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn decode_status_details(status: &tonic::Status) -> Option<ProtoWireError> {
    let details = status.details();
    if details.is_empty() {
        return None;
    }
    ProtoWireError::decode(details).ok()
}

#[cfg(test)]
mod tests {
    use super::{ClientError, ErrorDetail};

    fn assert_send_sync_static<T: Send + Sync + 'static>() {}

    #[test]
    fn client_error_is_send_sync_static() {
        assert_send_sync_static::<ClientError>();
    }

    /// Every variant of the taxonomy, exercised so adding a variant breaks
    /// this list until its class/Display contract is pinned.
    fn all_variants() -> Vec<ClientError> {
        vec![
            ClientError::not_found("d"),
            ClientError::already_exists("d"),
            ClientError::query_failed("d"),
            ClientError::query_timeout("d"),
            ClientError::unknown_query("d"),
            ClientError::not_running("d"),
            ClientError::cancelled("d"),
            ClientError::unavailable("d"),
            ClientError::unauthenticated("d"),
            ClientError::namespace_denied("d"),
            ClientError::invalid_argument("d"),
            ClientError::server("d"),
        ]
    }

    #[test]
    fn display_is_class_colon_detail_for_every_variant() {
        let mut classes = Vec::new();
        for error in all_variants() {
            assert_eq!(
                error.to_string(),
                format!("{}: d", error.class()),
                "{error:?} Display must be `<class>: <detail>`",
            );
            assert_eq!(error.detail().message, "d");
            classes.push(error.class());
        }
        let expected = [
            "not_found",
            "already_exists",
            "query_failed",
            "query_timeout",
            "unknown_query",
            "not_running",
            "cancelled",
            "unavailable",
            "unauthenticated",
            "namespace_denied",
            "invalid_input",
            "backend",
        ];
        assert_eq!(classes, expected, "class strings are a pinned contract");
    }

    #[test]
    fn detail_display_appends_the_typed_discriminator() {
        assert_eq!(ErrorDetail::new("plain").to_string(), "plain");
        assert_eq!(
            ErrorDetail::with_type("store unavailable", "Durability").to_string(),
            "store unavailable [Durability]"
        );
        assert_eq!(
            ClientError::not_found(ErrorDetail::with_type(
                "workflow was not found",
                "WorkflowNotFound"
            ))
            .to_string(),
            "not_found: workflow was not found [WorkflowNotFound]"
        );
    }
}
