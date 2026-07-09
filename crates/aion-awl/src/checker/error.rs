use crate::Span;

/// A typechecker diagnostic with a source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    /// The source span for the offending expression or name.
    pub span: Span,
    /// Human-readable diagnostic text.
    pub message: String,
}

impl CheckError {
    pub(super) fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}
