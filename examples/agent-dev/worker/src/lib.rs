//! Library surface of the agent-dev worker: the wire types, the typed
//! shell-out boundary, and the plain activity handler bodies.
//!
//! The binary (`main.rs`) is a thin composition root that registers the
//! handlers with the `aion-worker` SDK and composes the Norn agent harness;
//! the hermetic test suites drive the same handler functions directly with
//! fake-CLI shims on a private `PATH`.

pub mod handlers;
pub mod shell;
pub mod types;
