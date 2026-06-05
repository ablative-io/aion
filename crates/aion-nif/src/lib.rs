//! Rust helpers for declaring native functions for Gleam and Elixir workflows.

pub mod error;
pub mod raw;
pub mod term;
pub mod term_collection;

pub use error::{NifDeclError, TermError};
pub use term::{AtomName, FromTerm, IntoTerm};
