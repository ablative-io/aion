//! Authoring API error taxonomy and its wire mapping.
//!
//! Mirrors [`crate::api::handlers::deploy::DeployApiError`]: a small set of
//! failure classes the transports render distinctly. The defining class is
//! [`AuthoringApiError::TypeError`], which carries the verbatim `gleam`
//! compiler diagnostics so the author sees the real type error inline (HTTP
//! 400). Everything else maps onto the standard wire-code tables.

use aion_proto::WireError;

/// Failure classes for the server-side authoring loop.
#[derive(Debug)]
pub enum AuthoringApiError {
    /// The submitted source did not compile or type-check. The carried string
    /// is the verbatim `gleam` compiler output, returned inline so the author
    /// corrects against the real type-checker (rendered 400 by the HTTP
    /// facade).
    TypeError(String),
    /// The server is draining or the engine is shutting down (503).
    Unavailable(WireError),
    /// Mapped wire failure rendered through the standard code tables:
    /// authorization denials, spawn/packaging/load faults, and misconfiguration.
    Wire(WireError),
}

impl AuthoringApiError {
    /// A stable refusal-class label for audit lines and metrics.
    #[must_use]
    pub fn outcome(&self) -> &'static str {
        match self {
            Self::TypeError(_) => "type_error",
            Self::Unavailable(_) => "unavailable",
            Self::Wire(wire) => wire.code.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use aion_proto::WireError;

    use super::AuthoringApiError;

    #[test]
    fn outcome_labels_are_stable() {
        assert_eq!(
            AuthoringApiError::TypeError("error: bad".to_owned()).outcome(),
            "type_error"
        );
        assert_eq!(
            AuthoringApiError::Unavailable(WireError::backend("draining")).outcome(),
            "unavailable"
        );
        assert_eq!(
            AuthoringApiError::Wire(WireError::invalid_input("nope")).outcome(),
            WireError::invalid_input("nope").code.as_str()
        );
    }
}
