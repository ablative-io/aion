//! The canonical printer: one shape per document, lossless comments and
//! doc lines, `parse ∘ print = id` and `print ∘ parse ∘ print = print`.

mod document;
mod exprs;
mod steps;

pub use document::print;
