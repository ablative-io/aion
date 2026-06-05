//! Rust helpers for declaring native functions for Gleam and Elixir workflows.

#![deny(unsafe_code)]

pub mod declare;
pub mod descriptor;
pub mod error;
pub mod payload;
pub mod raw;
pub mod registry;
pub mod term;
pub mod term_collection;

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
