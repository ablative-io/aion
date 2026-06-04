//! timer module declarations + re-exports

pub mod named;
pub mod recovery;
pub mod timer_service;

pub use recovery::{TimerRecovery, TimerRecoveryError};
pub use timer_service::{TimerService, TimerServiceError};
