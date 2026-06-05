//! Rust helpers for declaring native functions for Gleam and Elixir workflows.

pub mod error;
pub mod raw;

pub use error::{NifDeclError, TermError};
