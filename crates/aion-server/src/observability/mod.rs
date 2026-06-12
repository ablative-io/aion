//! Server-level observability surfaces.

/// Health-check response helpers.
pub mod health;
/// Event-store decorator recording server metrics.
pub mod instrumented_store;
/// Metrics registry and rendering support.
pub mod metrics;
/// Tracing subscriber initialization support.
pub mod tracing;

pub use instrumented_store::InstrumentedEventStore;
pub use metrics::{Metrics, MetricsError};
