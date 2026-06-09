//! Deployable HTTP, gRPC, WebSocket, and worker endpoint for Aion workflows.
//!
//! This crate wraps the transport-agnostic engine with API handlers, namespace
//! isolation, observability, shutdown handling, dashboard assets, and
//! remote-worker task dispatch.
//!
//! # Example
//!
//! ```
//! use aion_server::ServerConfig;
//!
//! let config = ServerConfig::default();
//! println!("serving gRPC on {}", config.server.grpc_address);
//! ```

/// HTTP, gRPC, and worker API handlers.
pub mod api;
#[cfg(feature = "auth")]
/// Authentication middleware and token validation.
pub mod auth;
/// Runtime configuration loading and validation.
pub mod config;
/// Dashboard asset serving helpers.
pub mod dashboard;
/// Server error and stream-failure types.
pub mod error;
/// Namespace resolution and authorization guard types.
pub mod namespace;
/// Health, metrics, and tracing support.
pub mod observability;
/// Cooperative shutdown and drain handling.
pub mod shutdown;
/// Shared server state construction and access.
pub mod state;
/// WebSocket event-streaming support.
pub mod stream;
/// Remote-worker registry, heartbeat, and dispatch support.
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
