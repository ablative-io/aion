//! `WireError` taxonomy and mapping.
//!
//! `WireErrorCode` is the only client-branchable failure contract. The
//! associated message is informational and may change without notice.
//!
//! Authoritative mapping table for adapters that can see engine/store types:
//! - `aion_store::StoreError::SequenceConflict` -> `SequenceConflict`.
//! - `aion_store::StoreError::NotFound` -> `NotFound`.
//! - `aion_store::StoreError::Backend | Serialization` -> `Backend`.
//! - `aion::EngineError::WorkflowNotFound` -> `NotFound`.
//! - `aion::EngineError::Store | Durability(StoreError)` -> store mapping above.
//! - Other operational engine failures -> `Backend`.
//! - Query unknown/timeout/not-running/unknown-workflow ->
//!   `UnknownQuery`/`QueryTimeout`/`NotRunning`/`NotFound`.
//! - Query handler ran and reported an application-level failure ->
//!   `QueryFailed`. Query reply dropped because the workflow ended first ->
//!   `NotRunning`.
//! - Signal terminal/unknown target -> `NotRunning`/`NotFound`.
//! - Namespace authorization failure -> `NamespaceDenied`.
//! - Bounded subscriber overflow -> `Lagged`.
//!
//! This crate intentionally does not depend on `aion` or `aion-store` to keep
//! the proto crate leaf-safe; server-side adapters apply this documented table
//! where those concrete error types are reachable.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable, closed, client-branchable wire error codes.
///
/// The JSON representation is the `snake_case` code returned by
/// [`WireErrorCode::as_str`] — the documented stable contract every SDK wire
/// map branches on. `rename_all = "snake_case"` keeps Serialize/Deserialize
/// byte-identical to `as_str()` for every variant; the
/// `json_codes_match_as_str_and_round_trip` pin test enforces this.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WireErrorCode {
    /// The requested workflow, run, activity, timer, or history item was not found.
    NotFound,
    /// The caller is not authorized to operate in the requested namespace.
    NamespaceDenied,
    /// A durable write lost an optimistic sequence-position race.
    SequenceConflict,
    /// The requested workflow query name is not registered.
    UnknownQuery,
    /// A workflow query exceeded its configured timeout/window.
    QueryTimeout,
    /// The target workflow is terminal or otherwise not running.
    NotRunning,
    /// A bounded stream consumer fell behind and was disconnected.
    Lagged,
    /// A request body, identifier, or envelope is malformed or semantically invalid.
    InvalidInput,
    /// Backend storage, serialization, runtime, or other internal failure.
    Backend,
    /// The workflow's query handler ran and reported an application-level failure.
    QueryFailed,
    /// The caller is not authorized to use the operator deploy surface.
    DeployDenied,
    /// A deploy unload/route was refused because the version is route-active
    /// or pinned by live state.
    VersionPinned,
    /// The targeted shard is owned by a different cluster node; the request was
    /// fenced. A retryable routing signal: the caller (or the request-routing
    /// edge) should re-resolve the shard owner and retry or forward.
    NotOwner,
    /// A precondition on the target's current state was not met (e.g. a reopen
    /// of a run that is not a reopenable terminal). Distinct from `NotFound`
    /// (absent) and `Backend` (internal failure): the target exists but is in
    /// the wrong state. Maps to gRPC `FailedPrecondition` / HTTP 409 Conflict.
    InvalidState,
}

impl WireErrorCode {
    /// Returns the stable string code SDKs may branch on.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::NamespaceDenied => "namespace_denied",
            Self::SequenceConflict => "sequence_conflict",
            Self::UnknownQuery => "unknown_query",
            Self::QueryTimeout => "query_timeout",
            Self::NotRunning => "not_running",
            Self::Lagged => "lagged",
            Self::InvalidInput => "invalid_input",
            Self::Backend => "backend",
            Self::QueryFailed => "query_failed",
            Self::DeployDenied => "deploy_denied",
            Self::VersionPinned => "version_pinned",
            Self::NotOwner => "not_owner",
            Self::InvalidState => "invalid_state",
        }
    }
}

impl fmt::Display for WireErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Wire-safe error value. `code` is stable; `message` is informational only.
#[derive(thiserror::Error, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct WireError {
    /// Stable client-branchable error code.
    pub code: WireErrorCode,
    /// Human-readable informational message.
    pub message: String,
    /// Concrete typed error variant, when the server can expose one safely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

impl WireError {
    /// Creates a wire error with the supplied stable code and informational message.
    #[must_use]
    pub fn new(code: WireErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            error_type: None,
        }
    }

    /// Attach a concrete typed error variant name to this wire error.
    #[must_use]
    pub fn with_error_type(mut self, error_type: impl Into<String>) -> Self {
        self.error_type = Some(error_type.into());
        self
    }

    /// Attach an optional concrete typed error variant name to this wire error.
    #[must_use]
    pub fn with_optional_error_type(mut self, error_type: Option<String>) -> Self {
        self.error_type = error_type;
        self
    }

    /// Creates a wire error with a concrete typed error variant name.
    #[must_use]
    pub fn new_with_type(
        code: WireErrorCode,
        error_type: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(code, message).with_error_type(error_type)
    }

    /// Not-found failure.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::NotFound, message)
    }

    /// Namespace authorization failure.
    #[must_use]
    pub fn namespace_denied(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::NamespaceDenied, message)
    }

    /// Durable sequence conflict failure.
    #[must_use]
    pub fn sequence_conflict(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::SequenceConflict, message)
    }

    /// Unknown workflow query failure.
    #[must_use]
    pub fn unknown_query(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::UnknownQuery, message)
    }

    /// Query timeout failure.
    #[must_use]
    pub fn query_timeout(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::QueryTimeout, message)
    }

    /// Workflow not-running failure.
    #[must_use]
    pub fn not_running(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::NotRunning, message)
    }

    /// Lagged stream failure.
    #[must_use]
    pub fn lagged(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::Lagged, message)
    }

    /// Invalid input failure.
    #[must_use]
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::InvalidInput, message)
    }

    /// Backend/internal failure.
    #[must_use]
    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::Backend, message)
    }

    /// Query-handler application-level failure.
    #[must_use]
    pub fn query_failed(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::QueryFailed, message)
    }

    /// Deploy authorization failure.
    #[must_use]
    pub fn deploy_denied(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::DeployDenied, message)
    }

    /// Deploy version-pinned refusal.
    #[must_use]
    pub fn version_pinned(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::VersionPinned, message)
    }

    /// Wrong-shard-owner (fenced) failure — retryable routing signal.
    #[must_use]
    pub fn not_owner(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::NotOwner, message)
    }

    /// Invalid-state precondition failure.
    #[must_use]
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::InvalidState, message)
    }

    /// Invalid-state precondition failure with a concrete typed error variant name.
    #[must_use]
    pub fn invalid_state_with_type(
        error_type: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new_with_type(WireErrorCode::InvalidState, error_type, message)
    }

    /// Not-found failure with a concrete typed error variant name.
    #[must_use]
    pub fn not_found_with_type(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new_with_type(WireErrorCode::NotFound, error_type, message)
    }

    /// Not-running failure with a concrete typed error variant name.
    #[must_use]
    pub fn not_running_with_type(
        error_type: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new_with_type(WireErrorCode::NotRunning, error_type, message)
    }

    /// Backend/internal failure with a concrete typed error variant name.
    #[must_use]
    pub fn backend_with_type(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new_with_type(WireErrorCode::Backend, error_type, message)
    }
}

/// Proto representation of [`WireErrorCode`]. Zero is invalid on decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, prost::Enumeration)]
#[repr(i32)]
pub enum ProtoWireErrorCode {
    /// Missing/invalid code.
    Unspecified = 0,
    /// See [`WireErrorCode::NotFound`].
    NotFound = 1,
    /// See [`WireErrorCode::NamespaceDenied`].
    NamespaceDenied = 2,
    /// See [`WireErrorCode::SequenceConflict`].
    SequenceConflict = 3,
    /// See [`WireErrorCode::UnknownQuery`].
    UnknownQuery = 4,
    /// See [`WireErrorCode::QueryTimeout`].
    QueryTimeout = 5,
    /// See [`WireErrorCode::NotRunning`].
    NotRunning = 6,
    /// See [`WireErrorCode::Lagged`].
    Lagged = 7,
    /// See [`WireErrorCode::InvalidInput`].
    InvalidInput = 8,
    /// See [`WireErrorCode::Backend`].
    Backend = 9,
    /// See [`WireErrorCode::QueryFailed`].
    QueryFailed = 10,
    /// See [`WireErrorCode::DeployDenied`].
    DeployDenied = 11,
    /// See [`WireErrorCode::VersionPinned`].
    VersionPinned = 12,
    /// See [`WireErrorCode::NotOwner`].
    NotOwner = 13,
    /// See [`WireErrorCode::InvalidState`].
    InvalidState = 14,
}

/// Proto representation of [`WireError`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, prost::Message)]
pub struct ProtoWireError {
    /// Stable client-branchable code.
    #[prost(enumeration = "ProtoWireErrorCode", tag = "1")]
    pub code: i32,
    /// Informational message.
    #[prost(string, tag = "2")]
    pub message: String,
    /// Concrete typed error variant, when known.
    #[prost(string, optional, tag = "3")]
    pub error_type: Option<String>,
}

impl From<WireErrorCode> for ProtoWireErrorCode {
    fn from(value: WireErrorCode) -> Self {
        match value {
            WireErrorCode::NotFound => Self::NotFound,
            WireErrorCode::NamespaceDenied => Self::NamespaceDenied,
            WireErrorCode::SequenceConflict => Self::SequenceConflict,
            WireErrorCode::UnknownQuery => Self::UnknownQuery,
            WireErrorCode::QueryTimeout => Self::QueryTimeout,
            WireErrorCode::NotRunning => Self::NotRunning,
            WireErrorCode::Lagged => Self::Lagged,
            WireErrorCode::InvalidInput => Self::InvalidInput,
            WireErrorCode::Backend => Self::Backend,
            WireErrorCode::QueryFailed => Self::QueryFailed,
            WireErrorCode::DeployDenied => Self::DeployDenied,
            WireErrorCode::VersionPinned => Self::VersionPinned,
            WireErrorCode::NotOwner => Self::NotOwner,
            WireErrorCode::InvalidState => Self::InvalidState,
        }
    }
}

impl TryFrom<ProtoWireErrorCode> for WireErrorCode {
    type Error = WireError;

    fn try_from(value: ProtoWireErrorCode) -> Result<Self, Self::Error> {
        match value {
            ProtoWireErrorCode::Unspecified => {
                Err(WireError::backend("wire error code is missing"))
            }
            ProtoWireErrorCode::NotFound => Ok(Self::NotFound),
            ProtoWireErrorCode::NamespaceDenied => Ok(Self::NamespaceDenied),
            ProtoWireErrorCode::SequenceConflict => Ok(Self::SequenceConflict),
            ProtoWireErrorCode::UnknownQuery => Ok(Self::UnknownQuery),
            ProtoWireErrorCode::QueryTimeout => Ok(Self::QueryTimeout),
            ProtoWireErrorCode::NotRunning => Ok(Self::NotRunning),
            ProtoWireErrorCode::Lagged => Ok(Self::Lagged),
            ProtoWireErrorCode::InvalidInput => Ok(Self::InvalidInput),
            ProtoWireErrorCode::Backend => Ok(Self::Backend),
            ProtoWireErrorCode::QueryFailed => Ok(Self::QueryFailed),
            ProtoWireErrorCode::DeployDenied => Ok(Self::DeployDenied),
            ProtoWireErrorCode::VersionPinned => Ok(Self::VersionPinned),
            ProtoWireErrorCode::NotOwner => Ok(Self::NotOwner),
            ProtoWireErrorCode::InvalidState => Ok(Self::InvalidState),
        }
    }
}

impl From<WireError> for ProtoWireError {
    fn from(value: WireError) -> Self {
        let code = ProtoWireErrorCode::from(value.code) as i32;
        Self {
            code,
            message: value.message,
            error_type: value.error_type,
        }
    }
}

impl TryFrom<ProtoWireError> for WireError {
    type Error = WireError;

    fn try_from(value: ProtoWireError) -> Result<Self, Self::Error> {
        let code = ProtoWireErrorCode::try_from(value.code)
            .map_err(|_| WireError::backend("wire error code is unknown"))?;
        Ok(Self::new(WireErrorCode::try_from(code)?, value.message)
            .with_optional_error_type(value.error_type))
    }
}

#[cfg(test)]
mod tests {
    use super::{ProtoWireError, ProtoWireErrorCode, WireError, WireErrorCode};

    fn assert_send_sync<T: Send + Sync>() {}

    /// Exhaustive successor chain over [`WireErrorCode`]. Adding a variant
    /// makes this match non-exhaustive, so the build breaks until the new
    /// variant is threaded into the chain and therefore into every test that
    /// iterates [`all_codes`]. This is deliberately not a hand-maintained
    /// list.
    const fn next_code(code: WireErrorCode) -> Option<WireErrorCode> {
        match code {
            WireErrorCode::NotFound => Some(WireErrorCode::NamespaceDenied),
            WireErrorCode::NamespaceDenied => Some(WireErrorCode::SequenceConflict),
            WireErrorCode::SequenceConflict => Some(WireErrorCode::UnknownQuery),
            WireErrorCode::UnknownQuery => Some(WireErrorCode::QueryTimeout),
            WireErrorCode::QueryTimeout => Some(WireErrorCode::NotRunning),
            WireErrorCode::NotRunning => Some(WireErrorCode::Lagged),
            WireErrorCode::Lagged => Some(WireErrorCode::InvalidInput),
            WireErrorCode::InvalidInput => Some(WireErrorCode::Backend),
            WireErrorCode::Backend => Some(WireErrorCode::QueryFailed),
            WireErrorCode::QueryFailed => Some(WireErrorCode::DeployDenied),
            WireErrorCode::DeployDenied => Some(WireErrorCode::VersionPinned),
            WireErrorCode::VersionPinned => Some(WireErrorCode::NotOwner),
            WireErrorCode::NotOwner => Some(WireErrorCode::InvalidState),
            WireErrorCode::InvalidState => None,
        }
    }

    /// Every wire error code, derived from the compile-breaking chain above.
    fn all_codes() -> Vec<WireErrorCode> {
        let mut codes = vec![WireErrorCode::NotFound];
        while let Some(&last) = codes.last() {
            match next_code(last) {
                Some(next) => codes.push(next),
                None => break,
            }
        }
        codes
    }

    #[test]
    fn wire_error_is_send_sync() {
        assert_send_sync::<WireError>();
    }

    /// The numeric proto enum values are the cross-SDK wire contract: every
    /// generated decoder (Python, TypeScript, gRPC stubs) branches on these
    /// exact integers, so each variant's number is pinned explicitly.
    #[test]
    fn proto_numeric_values_are_pinned() {
        let expected: &[(WireErrorCode, i32)] = &[
            (WireErrorCode::NotFound, 1),
            (WireErrorCode::NamespaceDenied, 2),
            (WireErrorCode::SequenceConflict, 3),
            (WireErrorCode::UnknownQuery, 4),
            (WireErrorCode::QueryTimeout, 5),
            (WireErrorCode::NotRunning, 6),
            (WireErrorCode::Lagged, 7),
            (WireErrorCode::InvalidInput, 8),
            (WireErrorCode::Backend, 9),
            (WireErrorCode::QueryFailed, 10),
            (WireErrorCode::DeployDenied, 11),
            (WireErrorCode::VersionPinned, 12),
            (WireErrorCode::NotOwner, 13),
            (WireErrorCode::InvalidState, 14),
        ];
        assert_eq!(
            expected.len(),
            all_codes().len(),
            "every WireErrorCode variant must have a pinned numeric value"
        );
        for &(code, number) in expected {
            assert_eq!(
                ProtoWireErrorCode::from(code) as i32,
                number,
                "{code:?} must keep proto enum value {number}",
            );
        }
    }

    /// The `snake_case` string codes are the JSON wire contract every SDK
    /// branches on; each one is pinned explicitly.
    #[test]
    fn string_codes_are_pinned() {
        let expected: &[(WireErrorCode, &str)] = &[
            (WireErrorCode::NotFound, "not_found"),
            (WireErrorCode::NamespaceDenied, "namespace_denied"),
            (WireErrorCode::SequenceConflict, "sequence_conflict"),
            (WireErrorCode::UnknownQuery, "unknown_query"),
            (WireErrorCode::QueryTimeout, "query_timeout"),
            (WireErrorCode::NotRunning, "not_running"),
            (WireErrorCode::Lagged, "lagged"),
            (WireErrorCode::InvalidInput, "invalid_input"),
            (WireErrorCode::Backend, "backend"),
            (WireErrorCode::QueryFailed, "query_failed"),
            (WireErrorCode::DeployDenied, "deploy_denied"),
            (WireErrorCode::VersionPinned, "version_pinned"),
            (WireErrorCode::NotOwner, "not_owner"),
            (WireErrorCode::InvalidState, "invalid_state"),
        ];
        assert_eq!(
            expected.len(),
            all_codes().len(),
            "every WireErrorCode variant must have a pinned string code"
        );
        for &(code, string) in expected {
            assert_eq!(code.as_str(), string, "{code:?} must keep code {string}");
        }
    }

    #[test]
    fn json_codes_match_as_str_and_round_trip() -> Result<(), serde_json::Error> {
        for code in all_codes() {
            let serialized = serde_json::to_value(code)?;
            assert_eq!(
                serialized,
                serde_json::Value::String(code.as_str().to_owned()),
                "JSON serialization of {code:?} must equal as_str()",
            );
            let deserialized: WireErrorCode =
                serde_json::from_value(serde_json::Value::String(code.as_str().to_owned()))?;
            assert_eq!(deserialized, code, "{code:?} must round-trip through JSON");

            let error = WireError::new(code, format!("message for {}", code.as_str()));
            let body = serde_json::to_value(&error)?;
            assert_eq!(
                body.get("code"),
                Some(&serde_json::Value::String(code.as_str().to_owned())),
                "WireError JSON body must carry the snake_case code for {code:?}",
            );
            let decoded: WireError = serde_json::from_value(body)?;
            assert_eq!(decoded, error);
        }
        Ok(())
    }

    #[test]
    fn proto_round_trips_every_code() -> Result<(), WireError> {
        for code in all_codes() {
            let error = WireError::new_with_type(
                code,
                format!("{}Variant", code.as_str()),
                format!("message for {}", code.as_str()),
            );
            let proto = ProtoWireError::from(error.clone());
            let decoded = WireError::try_from(proto)?;
            assert_eq!(decoded, error);
        }

        Ok(())
    }

    #[test]
    fn rejects_unspecified_proto_code() {
        let proto = ProtoWireError {
            code: 0,
            message: String::from("missing"),
            error_type: None,
        };

        let result = WireError::try_from(proto);
        assert_eq!(
            result,
            Err(WireError::backend("wire error code is missing"))
        );
    }

    #[test]
    fn representative_documented_mappings_use_stable_codes() {
        let engine_unknown_workflow = WireError::not_found("workflow was not found");
        let store_sequence_conflict = WireError::sequence_conflict("event sequence conflicted");

        assert_eq!(engine_unknown_workflow.code, WireErrorCode::NotFound);
        assert_eq!(
            store_sequence_conflict.code,
            WireErrorCode::SequenceConflict
        );
        assert_eq!(
            WireError::namespace_denied("denied").code,
            WireErrorCode::NamespaceDenied
        );
        assert_eq!(
            WireError::query_timeout("timeout").code,
            WireErrorCode::QueryTimeout
        );
        assert_eq!(
            WireError::unknown_query("unknown").code,
            WireErrorCode::UnknownQuery
        );
        assert_eq!(
            WireError::not_running("terminal").code,
            WireErrorCode::NotRunning
        );
        assert_eq!(
            WireError::invalid_input("malformed").code,
            WireErrorCode::InvalidInput
        );
        assert_eq!(
            WireError::query_failed("handler raised").code,
            WireErrorCode::QueryFailed
        );
        assert_eq!(
            WireError::deploy_denied("no deploy grant").code,
            WireErrorCode::DeployDenied
        );
        assert_eq!(
            WireError::version_pinned("pinned by live run").code,
            WireErrorCode::VersionPinned
        );
        assert_eq!(
            WireError::not_owner("wrong shard owner").code,
            WireErrorCode::NotOwner
        );
        assert_eq!(
            WireError::invalid_state("run is not reopenable").code,
            WireErrorCode::InvalidState
        );
    }
}
