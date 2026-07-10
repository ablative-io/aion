//! Library surface of the awl-hello worker: the two activity wire types and
//! their pure handler bodies.
//!
//! The binary (`main.rs`) is the composition root: it registers the two
//! handlers with the `aion-worker` SDK on the `awl_hello` task queue (node
//! `hello`, locality metadata — the workflow's dispatches are unpinned).
//! The unit tests drive the same handler functions directly.

pub mod activities;
