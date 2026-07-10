//! Schema-derivation stub awaiting the rev-2 rebuild (AWL-2 build plan,
//! phase 4, alongside the typechecker). The AWL-0 derivation lives in git
//! history (`git show 9bf366b7 -- crates/aion-awl/src/schema/`).

use serde_json::Value;
use thiserror::Error;

use crate::Span;
use crate::ast::Document;

/// Failure to derive a JSON Schema from an AWL contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchemaError {
    /// Derivation is not available until the rev-2 rebuild lands.
    #[error(
        "schema derivation is being rebuilt against the rev-2 surface \
         (AWL-2 build plan, phase 4) and cannot derive yet"
    )]
    PendingRebuild {
        /// Document span used to anchor the diagnostic.
        span: Span,
    },
}

impl SchemaError {
    /// Span that anchors a compiler-style diagnostic.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::PendingRebuild { span } => *span,
        }
    }
}

/// Derive the JSON Schema for a named declared type.
///
/// # Errors
///
/// Stub: always refuses until the rev-2 derivation lands (phase 4).
pub fn schema_for_type(document: &Document, _name: &str) -> Result<Value, SchemaError> {
    Err(SchemaError::PendingRebuild {
        span: document.span,
    })
}

/// Derive the JSON Schema for the workflow's input contract.
///
/// # Errors
///
/// Stub: always refuses until the rev-2 derivation lands (phase 4).
pub fn schema_for_workflow(document: &Document) -> Result<Value, SchemaError> {
    Err(SchemaError::PendingRebuild {
        span: document.span,
    })
}
