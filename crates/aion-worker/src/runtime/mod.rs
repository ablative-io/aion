//! Worker activity dispatch runtime support.

/// Typed activity dispatch and payload conversion helpers.
pub mod dispatch;
/// Liminal worker transport (LSUB-1): receive pushed dispatches, execute, reply.
#[cfg(feature = "liminal-transport")]
pub mod liminal;
/// Candidate-cycling redial driver for liminal worker reconnect-to-survivor
/// (G-1, #112): the transport-free cursor + backoff + loop the liminal worker
/// uses to migrate from a dead owner to a survivor's listener.
#[cfg(feature = "liminal-transport")]
pub mod liminal_redial;
/// Reconnect-to-survivor serve entry point: wires the real liminal worker connect
/// + serve into the redial driver (G-1, #112).
#[cfg(feature = "liminal-transport")]
pub mod liminal_serve;
/// Activity polling loop and shutdown primitives.
pub mod loop_;
/// Dispatch-outcome reporting and runtime-channel draining.
pub(crate) mod report;

pub use dispatch::{TypedActivityDispatcher, decode_payload, encode_payload};
#[cfg(feature = "liminal-transport")]
pub use liminal::{DispatchRequest, DispatchResponse, LiminalActivityWorker};
#[cfg(feature = "liminal-transport")]
pub use liminal_serve::serve_with_redial;
pub use loop_::{
    ActivityDispatcher, DispatchOutcome, NoShutdown, ServeEnd, SessionHealth, serve_activity_tasks,
    serve_activity_tasks_until,
};
