//! Rust remote-worker SDK for executing Aion activities over gRPC.
//!
//! The SDK registers typed activity handlers, receives pushed tasks from an
//! `aion-server`, executes them out-of-process, reports results, and sends
//! heartbeats for long-running work.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Deserialize, Serialize)]
//! struct Input { name: String }
//!
//! #[derive(Serialize)]
//! struct Output { message: String }
//!
//! fn greet(input: Input, _context: &ActivityContext) -> HandlerFuture<'_, Output> {
//!     Box::pin(async move { Ok(Output { message: format!("hello, {}", input.name) }) })
//! }
//!
//! let config = WorkerConfig::builder()
//!     .endpoint("http://127.0.0.1:50051")
//!     .task_queue("default")
//!     .identity("rust-worker-1")
//!     .max_concurrency(4)
//!     .reconnect_initial_backoff(std::time::Duration::from_millis(500))
//!     .reconnect_max_backoff(std::time::Duration::from_secs(5))
//!     .reconnect_max_attempts(10)
//!     .build()?;
//!
//! Worker::builder(config)
//!     .register_activity("examples.greet", greet)?
//!     .build()?
//!     .run()
//!     .await?;
//! # Ok(())
//! # }
//! ```

/// Typed activity registration and failure classification.
pub mod activity;
/// Worker endpoint, identity, transport, and reconnect configuration.
pub mod config;
/// Per-activity execution context, heartbeat, and cancellation handles.
pub mod context;
/// Worker runtime and configuration errors.
pub mod error;
/// Worker-session protocol abstractions and task types.
pub mod protocol;
/// Activity dispatch and task-serving loops.
pub mod runtime;
/// High-level worker builder and run loop.
pub mod worker;

pub use activity::{
    ActivityFailure, ActivityRegistry, Classification, DuplicateActivityType, HandlerFuture,
};
pub use config::{
    ReconnectConfig, TransportCredentials, WorkerConfig, WorkerConfigBuildError,
    WorkerConfigBuilder,
};
pub use context::{ActivityCancellationHandle, ActivityContext, HeartbeatRequest};
pub use error::{MissingActivityHandler, WorkerError};
pub use protocol::{
    ActivityTask, GrpcWorkerSession, PendingActivityReport, ReconnectBackoff,
    RegisteredSessionInfo, UnackedResultTracker, WorkerSession, WorkerSessionEvent,
    WorkerTaskStream, connect_registered_grpc_session, re_report_unacked, reconnect_with_backoff,
    reconnect_with_sleep, register_connected_session, validate_activity_handlers,
};
#[cfg(feature = "liminal-transport")]
pub use runtime::liminal::{DispatchRequest, DispatchResponse, LiminalActivityWorker};
#[cfg(feature = "liminal-transport")]
pub use runtime::serve_with_redial;
pub use runtime::{
    ActivityDispatcher, DispatchOutcome, NoShutdown, ServeEnd, SessionHealth,
    TypedActivityDispatcher, decode_payload, encode_payload, serve_activity_tasks,
    serve_activity_tasks_until,
};
pub use worker::{EmptyActivitySet, Worker, WorkerBuilder, run_worker_with_session};
