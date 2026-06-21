//! Workflow structure projection: a graph model derived from the typed source.
//!
//! A workflow is a typed function over a small, known vocabulary
//! (`run` / `all` / `race` / `map` / `spawn` / `receive` / `sleep` / timers), so
//! its primitive structure can be projected from the entry-module Gleam source
//! into an ordered node/edge graph automatically (C21, C23). Each node carries a
//! [`CorrelationKey`] so a consumer (the dashboard canvas, RM-007) can overlay a
//! run's recorded events onto the graph (C22). A bounded structural delta
//! regenerates Gleam that still type-checks (C24).
//!
//! The graph model is a projection, never the authoritative artifact: the typed
//! Gleam module remains the single source of truth (ADR-014, CN6). This module
//! delivers the data model and the regeneration only — the rendered canvas UI
//! and the live overlay are deferred to RM-007.

mod error;
mod extract;
mod ident;
mod model;
mod regen;
mod scan;

#[cfg(test)]
mod tests;

pub use error::StructureError;
pub use extract::extract_structure;
pub use model::{
    CorrelationKey, EdgeKind, GraphEdge, GraphNode, NodeId, NodePrimitive, WorkflowGraph,
};
pub use regen::{StructuralDelta, regenerate_gleam};
