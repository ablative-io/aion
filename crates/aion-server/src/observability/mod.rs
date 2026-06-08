//! Server-level observability surfaces.

pub mod health;
pub mod metrics;

pub use metrics::{Metrics, MetricsError};
