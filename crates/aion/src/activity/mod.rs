//! Activity dispatch bridges and helpers.

/// Shared dispatcher installation and invocation bridge.
pub mod bridge;
/// Activity outcome dispatch and error propagation helpers.
pub mod dispatch;

pub use bridge::{ActivityDispatch, ActivityDispatcher};
pub use dispatch::{dispatch_activity, propagate_activity_outcome, surface_activity_error};
