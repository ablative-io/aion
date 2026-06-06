//! The Rust remote-worker SDK. Registers activity types, receives pushed tasks over the gRPC worker protocol, executes them out-of-process, reports results and heartbeats.

pub mod activity;
pub mod config;
pub mod context;
pub mod error;
pub mod protocol;
pub mod runtime;
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
    ActivityTask, GrpcWorkerSession, WorkerSession, WorkerSessionEvent, WorkerTaskStream,
    validate_activity_handlers,
};
pub use runtime::{
    ActivityDispatcher, DispatchOutcome, NoShutdown, TypedActivityDispatcher, decode_payload,
    encode_payload, serve_activity_tasks, serve_activity_tasks_until,
};
pub use worker::{EmptyActivitySet, Worker, WorkerBuilder, run_worker_with_session};
