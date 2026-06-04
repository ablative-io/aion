//! timer module declarations + re-exports

pub mod named;
pub mod recovery;
pub mod timer_service;

pub use named::{SleepTimer, SleepTimerError, cancel_timer, sleep, start_timer};
pub use timer_service::{TimerService, TimerServiceError};
