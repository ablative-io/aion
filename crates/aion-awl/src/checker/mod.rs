//! Typechecker for the rev-2 surface: call contracts, binding flow along
//! the graph, graph shape (targets, reachability, bounded cycles), outcome
//! exhaustiveness, guard-dependent optionality, and schema projection.

mod anchor;
mod args;
mod avail;
mod blocks;
mod collections;
mod context;
mod decls;
mod entry;
mod error;
mod exprs;
mod graph;
mod outcomes;
mod project;
mod stages;
mod types;
mod walk;

pub(crate) use entry::analyze;
pub use entry::{check, check_in};
pub use error::CheckError;
