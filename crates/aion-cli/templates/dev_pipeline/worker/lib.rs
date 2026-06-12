//! Library surface of the dev-pipeline worker: the wire types, the typed
//! shell-out boundary, and the activity handler bodies.
//!
//! The binary (`main.rs`) is a thin entry point that registers the handlers
//! with the `aion-worker` SDK; the hermetic test suite drives the same
//! handler functions directly with fake-CLI shims on a private `PATH`.

pub mod handlers;
pub mod shell;
pub mod types;
