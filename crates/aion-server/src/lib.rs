//! Deployable HTTP, gRPC, WebSocket, and worker endpoint for Aion workflows.
//!
//! This crate wraps the transport-agnostic engine with API handlers, namespace
//! isolation, observability, shutdown handling, ops-console assets, and
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
/// Transcript retention bounds: per-event size truncation for the durable `O`
/// keyspace (`[observability]` config).
pub(crate) mod activity_bounds;
/// NOI-5 transcript sequencer + live fan-out — commit-allocated `store_seq` over
/// the durable `O` keyspace, plus the resumable transcript broadcast.
pub mod activity_publisher;
pub mod api;
#[cfg(feature = "auth")]
/// Authentication middleware and token validation.
pub mod auth;
/// Server-side Gleam authoring surface (compile, type-check, package, hot-load).
pub mod authoring;
/// In-process AWL checking, formatting, semantics, and workspace documents.
pub(crate) mod awl;
/// SS-5b automatic multi-node failover detection (cluster supervisor). Only
/// meaningful for a distributed haematite boot, so gated behind the backend.
#[cfg(feature = "haematite-backend")]
pub mod cluster;
/// WS3 cluster topology/ownership broadcast publisher (not backend-gated: the
/// ops console's cluster channel is served on every boot, showing calm state with
/// no peers on a single-node server).
pub mod cluster_publisher;
/// Runtime configuration loading and validation.
pub mod config;
/// Operator deploy surface authorization.
pub mod deploy;
/// Local dev-server surface: trigger a run, stream it over the existing
/// firehose, mock a named activity per-run, and replay a failed run — all over
/// the real engine, store, and event stream.
pub mod dev_ui;
/// Server error and stream-failure types.
pub mod error;
/// Descriptor-relative filesystem confinement for sensitive server roots.
pub(crate) mod filesystem;
/// Engine-internal workflow filtering for enumeration surfaces.
mod internal_workflow;
/// Namespace resolution and authorization guard types.
pub mod namespace;
/// Health, metrics, and tracing support.
pub mod observability;
/// Ops-console asset serving helpers.
pub mod ops_console;
/// Distributed request routing: the gRPC-edge availability layer over the fence.
pub mod routing;
/// Server run loop: configuration load, transports, and graceful shutdown.
pub mod run;
/// Cooperative shutdown and drain handling.
pub mod shutdown;
/// Shared server state construction and access.
pub mod state;
/// WebSocket event-streaming support.
pub mod stream;
/// Remote-worker registry, heartbeat, and dispatch support.
pub mod worker;

pub use config::ServerConfig;
pub use deploy::DeployGuard;
pub use dev_ui::{ActivityMockRegistry, DevMockingDispatcher, MockedActivity};
pub use error::{ServerError, StreamFailure};
pub use namespace::{
    CallerIdentity, NAMESPACE_ATTRIBUTE, NamespaceGuard, NamespaceMinter, NamespaceOperation,
    NamespaceResolver, ScheduleNamespaceSource, ScheduleTarget, ScopedEngine,
    StaticScheduleNamespaces, StaticWorkflowNamespaces, SubscriptionScope, TASK_QUEUE_ATTRIBUTE,
    WorkflowAttribution, WorkflowNamespaceSource, WorkflowTarget,
};
pub use run::run;
pub use state::ServerState;
pub use worker::{
    HeartbeatSweeper, HeartbeatTracker, HeartbeatUpdate, InFlightActivity, LostWorkerReport,
    TaskLiveness,
};
