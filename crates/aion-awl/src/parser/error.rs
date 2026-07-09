use std::error::Error;
use std::fmt;

use crate::{LexError, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
/// A lexical or syntactic error encountered while parsing an AWL source document.
pub struct ParseError {
    /// The source location that identifies the fragment responsible for the parse failure.
    pub span: Span,
    /// The human-readable diagnostic describing why parsing could not continue.
    pub message: String,
}

impl ParseError {
    pub(super) fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at line {}, column {}",
            self.message, self.span.line, self.span.column
        )
    }
}

impl Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(value: LexError) -> Self {
        Self::new(value.span, value.message)
    }
}
