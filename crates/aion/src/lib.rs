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
pub mod signal;
pub mod supervision;
pub mod time;

pub use activity::{dispatch_activity, propagate_activity_outcome, surface_activity_error};
pub use engine_seam::EngineHandle;
pub use error::EngineError;
pub use loader::{LoadedWorkflow, LoadedWorkflows, load_package};
pub use registry::{Registry, WorkflowHandle};
pub use runtime::{Pid, RuntimeConfig, RuntimeHandle, RuntimeInput};
pub use supervision::{
    EngineSupervisorId, SupervisionTree, TypeSupervisorId, TypeSupervisorNode, WorkflowNode,
};
