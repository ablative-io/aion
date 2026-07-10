//! Emit-time error type: every refusal carries a source-correct span and a
//! message that says what could not be lowered and, where possible, what to
//! do instead.

use std::error::Error;
use std::fmt;

use crate::Span;

/// An error produced while lowering a parsed AWL document to Gleam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    /// The span of the construct that could not be lowered.
    pub span: Span,
    /// What was wrong and, where possible, what to do instead.
    pub message: String,
}

impl EmitError {
    pub(super) fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at line {}, column {}",
            self.message, self.span.line, self.span.column
        )
    }
}

impl Error for EmitError {}
