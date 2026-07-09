//! Library surface of the dev-brief worker: the shell-activity wire types,
//! the typed git/command shell boundary, the embedded agent output schemas,
//! the startup-loaded role profiles, the per-role prompt assembly, the
//! profile-injecting harness wrapper, and the three shell-activity handler
//! bodies.
//!
//! The binary (`main.rs`) is the composition root: it installs one
//! `ProfiledNornHarness` per agent role (developer, reviewer) and registers
//! the shell handlers with the `aion-worker` SDK. The hermetic tests drive
//! the same handler functions directly.

pub mod commit;
pub mod handlers;
pub mod harness;
pub mod paths;
pub mod profiles;
pub mod prompts;
pub mod schemas;
pub mod shell;
pub mod types;
