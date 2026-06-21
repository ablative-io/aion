//! The `aion new` subcommand: scaffold a complete, immediately-buildable
//! Aion workflow project from an embedded template.

pub mod agent;
pub mod scaffold;
pub mod template;

pub use scaffold::{NewArgs, run};
