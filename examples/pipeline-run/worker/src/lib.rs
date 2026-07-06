//! Library surface of the pipeline-run worker: the shell-activity wire types,
//! the typed git/cargo shell boundary, the four driven-agent output schemas and
//! per-role system prompts, and the four shell-activity handler bodies.
//!
//! The binary (`main.rs`) is the composition root: it installs one per-role
//! `NornHarness` for each agent activity and registers the shell handlers with
//! the `aion-worker` SDK. The hermetic tests drive the same handler functions
//! directly with fake-CLI shims on a private `PATH`.

pub mod handlers;
pub mod prompts;
pub mod schemas;
pub mod shell;
pub mod types;
