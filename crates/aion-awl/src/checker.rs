//! Typechecker stub awaiting the rev-2 rebuild (AWL-2 build plan, phase 4).
//!
//! The parser owns grammar-level diagnostics; every semantic duty in the
//! spec's checker list (call contracts, binding flow, route targets,
//! reachability, cycle boundedness, outcome exhaustiveness, flow typing,
//! schema projection) lands here in the checker phase. Until then `check`
//! reports nothing so the parse-only pipelines (`fmt`) stay usable; the
//! pre-rebuild checker suite is deliberately red.

use crate::Span;
use crate::ast::Document;

/// A typechecker diagnostic with a source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    /// The source span for the offending expression or name.
    pub span: Span,
    /// Human-readable diagnostic text.
    pub message: String,
}

/// Typecheck a parsed document, returning every diagnostic found.
///
/// Stub: reports nothing until the rev-2 checker lands (phase 4).
#[must_use]
pub fn check(_document: &Document) -> Vec<CheckError> {
    Vec::new()
}
