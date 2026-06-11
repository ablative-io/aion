//! Worker activity dispatch runtime support.

/// Typed activity dispatch and payload conversion helpers.
pub mod dispatch;
/// Activity polling loop and shutdown primitives.
pub mod loop_;
/// Dispatch-outcome reporting and runtime-channel draining.
pub(crate) mod report;

pub use dispatch::{TypedActivityDispatcher, decode_payload, encode_payload};
pub use loop_::{
    ActivityDispatcher, DispatchOutcome, NoShutdown, ServeEnd, serve_activity_tasks,
    serve_activity_tasks_until,
};
