//! Worker activity dispatch runtime support.

/// Harness-blind trait driver (NOI-4): `spawn_agent` drives any `AgentHarness`.
pub mod agent;
/// Typed activity dispatch and payload conversion helpers.
pub mod dispatch;
/// Worker-side mid-run intervention delivery (NOI-6): the attempt back-index that
/// routes a pushed neutral command to the live session that owns its target.
pub mod intervention;
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

pub use agent::{
    ActivityEventSender, ControlMessage, ControlReceiver, harness_error_to_outcome, spawn_agent,
};
pub use dispatch::{TypedActivityDispatcher, decode_payload, encode_payload};
pub use intervention::{ControlRegistry, SessionGuard, SessionKey};
#[cfg(feature = "liminal-transport")]
pub use liminal::{
    DispatchRequest, DispatchResponse, InterventionReply, InterventionRequest,
    LiminalActivityWorker,
};
#[cfg(feature = "liminal-transport")]
pub use liminal_serve::serve_with_redial;
pub use loop_::{
    ActivityDispatcher, DispatchOutcome, NoShutdown, ServeEnd, SessionHealth, serve_activity_tasks,
    serve_activity_tasks_until,
};
