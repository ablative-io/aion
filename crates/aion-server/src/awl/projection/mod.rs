//! Graph projection of a parsed AWL document — the canvas's 1:1 picture.
//!
//! Every step is a node; every node is a step. Step kinds come from the
//! checker's semantic index (never re-derived here); this module only adds
//! the label text and edge geometry the canvas draws.

mod build;
mod types;

#[cfg(test)]
mod tests;

pub use build::build;
pub use types::{GraphProjection, ProjectionEdgeKind};
