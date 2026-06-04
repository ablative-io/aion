//! Sole beamr boundary for aion per D1; other modules use `RuntimeHandle`.

pub mod config;
pub mod handle;
pub mod nif;
pub mod payload;

pub use config::RuntimeConfig;
pub use handle::{Pid, RuntimeHandle, RuntimeInput};
pub use nif::{Mfa, NifEntry, NifRegistration};
