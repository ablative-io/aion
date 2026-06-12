//! Operator deploy surface: authorization guard for the deploy API.

/// Deploy authorization guard shared by both transports.
pub mod guard;

pub use guard::DeployGuard;
