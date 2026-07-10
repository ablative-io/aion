//! The canonical printer: one shape per document, lossless comments and
//! doc lines, `parse ∘ print = id` and `print ∘ parse ∘ print = print`.

mod document;
mod exprs;
mod steps;

use crate::ast::Document;

use document::{Printer, print_document};

/// Render a parsed document in the canonical rev-2 format.
#[must_use]
pub fn print(document: &Document) -> String {
    let mut printer = Printer { out: String::new() };
    print_document(&mut printer, document);
    printer.out
}
