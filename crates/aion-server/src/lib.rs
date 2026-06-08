//! The standalone deployable. Wraps the transport-agnostic engine with HTTP/gRPC APIs, WebSocket event streaming, the remote-worker protocol endpoint, multi-tenancy, and dashboard hosting.

pub mod api;
#[cfg(feature = "auth")]
pub mod auth;
pub mod config;
pub mod dashboard;
pub mod error;
pub mod namespace;
pub mod shutdown;
pub mod state;
pub mod stream;
pub mod worker;

pub use config::ServerConfig;
pub use error::{ServerError, StreamFailure};
pub use namespace::{
    CallerIdentity, NamespaceGuard, NamespaceOperation, NamespaceResolver, ScopedEngine,
    SubscriptionScope, WorkflowOwnership, WorkflowTarget,
};
pub use state::ServerState;
pub use worker::{
    HeartbeatTracker, HeartbeatUpdate, InFlightActivity, LostWorkerReport, TaskLiveness,
};
