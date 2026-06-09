//! Sole beamr boundary for aion per D1; other modules use `RuntimeHandle`.

pub mod config;
mod engine_nifs;
pub mod handle;
pub mod module;
pub mod monitor;
pub mod nif;
mod nif_activity;
mod nif_activity_dispatch;
mod nif_child;
mod nif_child_engine;
mod nif_concurrency;
pub mod nif_context;
pub(crate) mod nif_determinism;
mod nif_query;
mod nif_query_mailbox;
mod nif_signal;
pub(crate) mod nif_timer;
pub mod outcome;
pub mod payload;
pub mod workflow;

pub use config::{RuntimeConfig, SignalDeliveryConfig};
pub use handle::{Pid, RuntimeHandle, RuntimeInput};
pub use nif::{Mfa, NifEntry, NifRegistration};
pub(crate) use nif_activity::install_nif_runtime_context;
pub(crate) use nif_child::install_child_nif_bridge;
pub(crate) use nif_child_engine::{ChildNifBridge, ChildNifBridgeParts};
pub(crate) use nif_query::install_query_bridge;
pub(crate) use nif_signal::{SignalNifBridge, install_signal_nif_bridge};
pub use outcome::WorkflowProcessOutcome;
