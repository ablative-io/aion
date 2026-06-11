//! Engine API, builder, and delegated seam surfaces.

/// High-level workflow engine API.
pub mod api;
/// Engine durable-schedule API surface and coordinator assembly.
mod api_schedule;
/// Engine construction and package source configuration.
pub mod builder;
/// Delegated signal, query, and event-publishing seams.
pub mod delegated;
/// `EngineHandle` seam implementation for the engine.
mod seam_handle;
/// Seam-assembly helpers used by `EngineBuilder::build()`.
mod seams;
/// Shutdown gating for in-flight lifecycle operations.
mod shutdown_gate;
/// Startup recovery wiring used by `EngineBuilder::build()`.
mod startup;

pub use api::Engine;
pub use builder::EngineBuilder;
pub use delegated::{
    DeferredEventPublisher, DeferredQueryService, DeferredSignalRouter, DelegatedSeams,
    EventFamily, EventFilter, EventPublisher, EventStreamLagged, QueryService, SignalRouter,
};
