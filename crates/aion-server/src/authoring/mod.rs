//! Server-side Gleam authoring surface: compile, type-check, package, and
//! hot-load submitted workflow source.
//!
//! Mounted only when `[authoring].gleam_path` is configured. The handlers are
//! transport-agnostic; the HTTP facade lives in `crate::api::http::authoring`.

/// Authoring API error taxonomy and wire mapping.
pub mod error;
/// Transport-agnostic compile-and-hot-load handler.
pub mod handlers;

pub use error::AuthoringApiError;
pub use handlers::{CompileSourceRequest, CompileSourceResponse, compile_and_load};
