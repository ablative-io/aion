//! Gleam-stopgap emitter stub awaiting the rev-2 port (AWL-2 build plan,
//! phase 5). The AWL-0 emitter it replaces lives in git history
//! (`git show 9bf366b7 -- crates/aion-awl/src/emitter/`); the port
//! re-targets it at the rev-2 canonical model rather than keeping the old
//! grammar alive. Emitting refuses loudly instead of lowering wrongly.

use std::error::Error;
use std::fmt;

use crate::Span;
use crate::ast::Document;

/// An error produced while lowering a parsed AWL document to Gleam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    /// The span of the construct that could not be lowered.
    pub span: Span,
    /// What was wrong and, where possible, what to do instead.
    pub message: String,
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
/// Stub: always refuses until the emitter is re-targeted at the rev-2
/// canonical model (phase 5).
pub fn emit(document: &Document) -> Result<String, EmitError> {
    Err(EmitError {
        span: document.span,
        message: "the Gleam emitter is being re-targeted at the rev-2 surface \
                  (AWL-2 build plan, phase 5) and cannot emit yet"
            .to_owned(),
    })
}
