use std::error::Error;
use std::fmt;

use super::Span;

/// A lexer diagnostic with a source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    /// The source span for the offending text.
    pub span: Span,
    /// Human-readable diagnostic text.
    pub message: String,
}

impl LexError {
    pub(super) fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = &self.message;
        let line = self.span.line;
        let column = self.span.column;
        write!(f, "{message} at line {line}, column {column}")
    }
}

impl Error for LexError {}
