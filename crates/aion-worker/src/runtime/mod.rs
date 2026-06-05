//! Module declarations.

pub mod dispatch;
pub mod loop_;

pub use dispatch::{TypedActivityDispatcher, decode_payload, encode_payload};
pub use loop_::{
    ActivityDispatcher, DispatchOutcome, serve_activity_tasks, serve_activity_tasks_with_reconnect,
    serve_activity_tasks_with_tracker,
};
