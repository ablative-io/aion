//! Worker activity dispatch runtime support.

/// Typed activity dispatch and payload conversion helpers.
pub mod dispatch;
/// Activity polling loop and shutdown primitives.
pub mod loop_;

pub use dispatch::{TypedActivityDispatcher, decode_payload, encode_payload};
pub use loop_::{
    ActivityDispatcher, DispatchOutcome, NoShutdown, serve_activity_tasks,
    serve_activity_tasks_until,
};
