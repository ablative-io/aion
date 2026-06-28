//! Worker activity dispatch runtime support.

/// Typed activity dispatch and payload conversion helpers.
pub mod dispatch;
/// Liminal worker transport (LSUB-1): receive pushed dispatches, execute, reply.
#[cfg(feature = "liminal-transport")]
pub mod liminal;
/// Activity polling loop and shutdown primitives.
pub mod loop_;
/// Dispatch-outcome reporting and runtime-channel draining.
pub(crate) mod report;

pub use dispatch::{TypedActivityDispatcher, decode_payload, encode_payload};
#[cfg(feature = "liminal-transport")]
pub use liminal::{DispatchRequest, DispatchResponse, LiminalActivityWorker};
pub use loop_::{
    ActivityDispatcher, DispatchOutcome, NoShutdown, ServeEnd, SessionHealth, serve_activity_tasks,
    serve_activity_tasks_until,
};
