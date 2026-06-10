//! Transport-agnostic Aion workflow engine with durability, replay, timers, and supervision.
//!
//! The engine embeds beamr, loads `.aion` packages, owns workflow lifecycle and
//! process residency, records and replays durable history, and exposes seams for
//! activities, events, signals, queries, and server transports.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//!
//! use aion::EngineBuilder;
//! use aion_store::{EventStore, InMemoryStore};
//!
//! let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
//! let engine = EngineBuilder::new()
//!     .store_arc(store)
//!     .in_memory_visibility()
//!     .build()
//!     .await?;
//! # let _ = engine;
//! # Ok(())
//! # }
//! ```

#![deny(unsafe_code)]

/// Activity dispatch bridge and error propagation helpers.
pub mod activity;
/// Child-workflow spawn support.
pub mod child;
/// Deterministic concurrency combinators for workflow code.
pub mod concurrency;
/// Durable command recording, replay, and recovery support.
pub mod durability;
/// Engine builder, runtime APIs, and delegated seams.
pub mod engine;
/// Handle type exposed by embedded engine seams.
pub mod engine_seam;
/// Engine and routing error types.
pub mod error;
/// Workflow lifecycle start, transition, visibility, and termination helpers.
pub mod lifecycle;
/// `.aion` package loading into runtime modules.
pub mod loader;
/// Query dispatch services and mailbox support.
pub mod query;
/// Active workflow registry and handle residency tracking.
pub mod registry;
/// BEAM runtime configuration, handles, NIFs, and workflow process support.
pub mod runtime;
/// Schedule evaluation and cron parsing support.
pub mod schedule;
/// Signal routing and resume handoff support.
pub mod signal;
/// Supervision tree models for engines, workflow types, and workflow instances.
pub mod supervision;
/// Timer creation, recovery, and wake-up services.
pub mod time;

pub use activity::{
    ActivityDispatcher, dispatch_activity, install_activity_dispatcher, propagate_activity_outcome,
    surface_activity_error,
};
pub use durability::ActiveWorkflowRecoverySeamImpl;
pub use engine::{
    DeferredEventPublisher, DeferredQueryService, DeferredSignalRouter, DelegatedSeams, Engine,
    EngineBuilder, EventFamily, EventFilter, EventPublisher, QueryService, SignalRouter,
};
pub use engine_seam::EngineHandle;
pub use error::{EngineError, SignalRouterError};
pub use loader::{LoadedWorkflow, LoadedWorkflows, load_package};
pub use registry::{
    CompletionNotifier, HandleResidency, Registry, Residency, TerminalOutcome, WorkflowHandle,
    WorkflowHandleParts,
};
pub use runtime::{Pid, RuntimeConfig, RuntimeHandle, RuntimeInput, SignalDeliveryConfig};
pub use schedule::{ScheduleError, next_fire_time, parse_cron_expression};
pub use supervision::{
    EngineSupervisorId, SupervisionTree, TypeSupervisorId, TypeSupervisorNode, WorkflowNode,
};
