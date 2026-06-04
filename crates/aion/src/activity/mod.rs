//! pub mod + re-exports only

pub mod dispatch;

pub use dispatch::{dispatch_activity, propagate_activity_outcome, surface_activity_error};
