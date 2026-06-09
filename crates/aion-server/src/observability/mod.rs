//! Server-level observability surfaces.

/// Health-check response helpers.
pub mod health;
/// Metrics registry and rendering support.
pub mod metrics;
/// Tracing subscriber initialization support.
pub mod tracing;

pub use metrics::{Metrics, MetricsError};
