//! Sole beamr boundary for aion per D1; other modules use `RuntimeHandle`.

/// Runtime configuration values passed to beamr.
pub mod config;
mod engine_nifs;
/// Runtime handle, process identifiers, and spawn input.
pub mod handle;
/// Loaded BEAM module representation.
pub mod module;
/// Runtime monitor utilities.
pub mod monitor;
/// Native-function registration records.
pub mod nif;
mod nif_activity;
mod nif_activity_dispatch;
mod nif_child;
mod nif_child_engine;
mod nif_concurrency;
/// Workflow NIF execution context.
pub mod nif_context;
mod nif_continue_as_new;
pub(crate) mod nif_determinism;
mod nif_query;
mod nif_query_mailbox;
mod nif_signal;
pub(crate) mod nif_state;
pub(crate) mod nif_timeout;
pub(crate) mod nif_timer;
/// Workflow process exit outcomes.
pub mod outcome;
/// Payload conversion helpers used at the runtime boundary.
pub mod payload;
/// Workflow module and entrypoint execution helpers.
pub mod workflow;

pub use config::{RuntimeConfig, SignalDeliveryConfig};
pub use handle::{Pid, RuntimeHandle, RuntimeInput};
pub use nif::{Mfa, NifEntry, NifRegistration};
pub(crate) use nif_activity::install_nif_runtime_context;
pub(crate) use nif_child::install_child_nif_bridge;
pub(crate) use nif_child_engine::{ChildNifBridge, ChildNifBridgeParts};
pub(crate) use nif_query::install_query_bridge;
pub(crate) use nif_signal::{SignalNifBridge, install_signal_nif_bridge};
pub(crate) use nif_state::EngineNifState;
pub use outcome::WorkflowProcessOutcome;
