//! Rust helpers for declaring native functions for Gleam and Elixir workflows.

pub mod error;
pub mod payload;
pub mod raw;
pub mod term;
pub mod term_collection;

pub use error::{NifDeclError, TermError};
pub use payload::{
    from_term_via_payload, into_term_via_payload, payload_from_term, payload_into_term,
};
pub use term::{AtomName, FromTerm, IntoTerm};
