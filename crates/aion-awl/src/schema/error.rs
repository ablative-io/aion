use thiserror::Error;

use crate::Span;

/// Failure to derive a JSON Schema from an AWL contract.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SchemaError {
    /// The requested type is not declared in the document.
    #[error("no type named `{name}` is declared")]
    UnknownType {
        /// The requested type name.
        name: String,
        /// Span of the workflow header, anchoring the diagnostic.
        span: Span,
    },
    /// A schema import cannot resolve without the document's directory.
    #[error("cannot resolve imported schema `{path}` without the document's directory")]
    ImportUnresolved {
        /// Import path as written.
        path: String,
        /// Span of the import path literal.
        span: Span,
    },
    /// A schema import could not be read from disk.
    #[error("cannot read imported schema `{path}`: {detail}")]
    ImportUnreadable {
        /// Import path as written.
        path: String,
        /// Operating-system error detail.
        detail: String,
        /// Span of the import path literal.
        span: Span,
    },
    /// A schema door's JSON does not parse.
    #[error("schema for `{name}` is not valid JSON: {detail}")]
    InvalidJson {
        /// The declared type name.
        name: String,
        /// JSON parser detail.
        detail: String,
        /// Span of the declaration.
        span: Span,
    },
}

impl SchemaError {
    /// Span that anchors a compiler-style diagnostic.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::UnknownType { span, .. }
            | Self::ImportUnresolved { span, .. }
            | Self::ImportUnreadable { span, .. }
            | Self::InvalidJson { span, .. } => *span,
        }
    }
}
