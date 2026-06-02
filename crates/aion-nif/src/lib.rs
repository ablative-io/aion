//! Rust helper crate for writing and registering NIFs that Gleam activities call. Deterministic helpers and recorded light in-VM activities, with the split enforced by type.

pub mod declare;
pub mod descriptor;
pub mod error;
pub mod payload;
pub mod raw;
pub mod registry;
pub mod term;
pub mod term_collection;
