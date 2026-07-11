//! Language Server Protocol support for the Aion Workflow Language.
//!
//! This crate is deliberately a protocol adapter: parsing, checking, and
//! canonical printing remain exclusively in `aion-awl`.

mod analysis;
mod navigation;
mod server;

pub use analysis::{byte_offset_at, diagnostics, format_document, position_at, range_for_span};
pub use server::{ServerError, run_connection, run_stdio};
