use std::error::Error;
use std::fmt;

use crate::{Document, Span};

use super::context::Emitter;

/// An error produced while lowering a parsed AWL document to Gleam.
///
/// Emission fails when a document uses a construct the emitter cannot lower
/// faithfully (rather than emitting panicking or non-compiling Gleam).
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

/// Emit a complete Gleam workflow module for a parsed AWL document.
///
/// # Errors
///
/// Returns [`EmitError`] when the document uses a construct that cannot be
/// lowered faithfully (for example `each` on a non-action step, a
/// `when`-guarded rebind of a name with no prior binding, or routing fields
/// on a child workflow call).
pub fn emit(document: &Document) -> Result<String, EmitError> {
    Emitter::new(document).emit()
}

/// The fixed error for referencing an opaque (untyped) child-workflow
/// result in a context that needs a real Gleam type.
pub(super) fn opaque_ref_error(name: &str, span: Span) -> EmitError {
    EmitError::new(
        span,
        format!(
            "child result `{name}` is untyped in this revision and cannot be used in expressions"
        ),
    )
}
