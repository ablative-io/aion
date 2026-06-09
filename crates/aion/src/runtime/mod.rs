//! Sole beamr boundary for aion per D1; other modules use `RuntimeHandle`.

pub mod config;
mod engine_nifs;
pub mod handle;
pub mod module;
pub mod monitor;
pub mod nif;
mod nif_activity;
pub mod nif_context;
pub(crate) mod nif_determinism;
pub(crate) mod nif_timer;
pub mod outcome;
pub mod payload;
pub mod workflow;

pub use config::RuntimeConfig;
pub use handle::{Pid, RuntimeHandle, RuntimeInput};
pub use nif::{Mfa, NifEntry, NifRegistration};
pub(crate) use nif_activity::install_nif_runtime_context;
pub use outcome::WorkflowProcessOutcome;
