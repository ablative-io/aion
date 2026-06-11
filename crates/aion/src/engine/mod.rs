//! Engine API, builder, and delegated seam surfaces.

/// High-level workflow engine API.
pub mod api;
/// Engine construction and package source configuration.
pub mod builder;
/// Delegated signal, query, and event-publishing seams.
pub mod delegated;
/// Startup recovery wiring used by `EngineBuilder::build()`.
mod startup;

pub use api::Engine;
pub use builder::EngineBuilder;
pub use delegated::{
    DeferredEventPublisher, DeferredQueryService, DeferredSignalRouter, DelegatedSeams,
    EventFamily, EventFilter, EventPublisher, EventStreamLagged, QueryService, SignalRouter,
};
