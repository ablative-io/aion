//! Sole beamr boundary for aion per D1; other modules use `RuntimeHandle`.

pub mod config;
pub mod handle;
pub mod nif;

pub use config::RuntimeConfig;
pub use handle::{Pid, RuntimeHandle, RuntimeInput};
