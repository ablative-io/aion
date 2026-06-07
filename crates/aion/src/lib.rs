//! The core engine. Embeds beamr; owns workflow lifecycle, process-per-workflow management, the supervision tree, .aion module loading, durability and replay (durability module), and timers/signals/queries/children/concurrency (time module). Transport-agnostic.

#![deny(unsafe_code)]

pub mod activity;
pub mod child;
pub mod concurrency;
pub mod durability;
pub mod engine;
pub mod engine_seam;
pub mod error;
pub mod lifecycle;
pub mod loader;
pub mod query;
pub mod registry;
pub mod runtime;
pub mod schedule;
pub mod signal;
pub mod supervision;
pub mod time;

pub use activity::{
    ActivityDispatcher, dispatch_activity, install_activity_dispatcher, propagate_activity_outcome,
    surface_activity_error,
};
pub use engine::{
    DeferredEventPublisher, DeferredQueryService, DeferredSignalRouter, DelegatedSeams, Engine,
    EngineBuilder, EventFamily, EventFilter, EventPublisher, QueryService, SignalRouter,
};
pub use engine_seam::EngineHandle;
pub use error::EngineError;
pub use loader::{LoadedWorkflow, LoadedWorkflows, load_package};
pub use registry::{
    CompletionNotifier, HandleResidency, Registry, Residency, TerminalOutcome, WorkflowHandle,
    WorkflowHandleParts,
};
pub use runtime::{Pid, RuntimeConfig, RuntimeHandle, RuntimeInput};
pub use schedule::{ScheduleError, next_fire_time, parse_cron_expression};
pub use supervision::{
    EngineSupervisorId, SupervisionTree, TypeSupervisorId, TypeSupervisorNode, WorkflowNode,
};
