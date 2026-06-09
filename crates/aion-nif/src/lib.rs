//! Native function declaration helpers for Gleam and Elixir Aion workflows.
//!
//! This crate provides typed BEAM term conversions, deterministic and activity
//! NIF descriptors, registry builders, and suspension handles used by workflow
//! runtimes that expose Rust functions to BEAM code.
//!
//! # Example
//!
//! ```
//! use aion_nif::{NifSet, deterministic_nif};
//!
//! fn double(value: i64) -> i64 {
//!     value * 2
//! }
//!
//! let nifs = NifSet::builder()
//!     .register(deterministic_nif!("math", "double", double, (value: i64) -> i64))?
//!     .build();
//! assert_eq!(nifs.len(), 1);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(unsafe_code)]

/// Process-scoped NIF conversion context.
pub mod context;
/// NIF declaration builders, public macros, and suspension helpers.
pub mod declare;
/// NIF descriptors and determinism classification.
pub mod descriptor;
/// NIF declaration and term-conversion errors.
pub mod error;
/// Payload-backed term conversion helpers.
pub mod payload;
/// Raw BEAM term conversion primitives.
pub mod raw;
/// Collections and builders for sets of declared NIFs.
pub mod registry;
/// Typed Rust-to-BEAM term conversion traits.
pub mod term;
/// Collection conversion support for BEAM terms.
pub mod term_collection;

pub use context::NifContext;
pub use declare::{
    ActivityWakeHandle, activity_descriptor, pure_descriptor, request_activity_suspend,
};
pub use descriptor::{Determinism, Nif};
pub use error::{NifDeclError, TermError};
pub use payload::{
    from_term_via_payload, into_term_via_payload, payload_from_term, payload_into_term,
};
pub use registry::{NifSet, NifSetBuilder};
pub use term::{AtomName, FromTerm, IntoTerm};
