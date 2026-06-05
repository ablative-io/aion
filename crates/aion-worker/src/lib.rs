//! The Rust remote-worker SDK. Registers activity types, receives pushed tasks over the gRPC worker protocol, executes them out-of-process, reports results and heartbeats.

pub mod activity;
pub mod config;
pub mod context;
pub mod error;
pub mod protocol;
pub mod runtime;
pub mod worker;

pub use config::{TransportCredentials, WorkerConfig, WorkerConfigBuildError, WorkerConfigBuilder};
pub use error::{MissingActivityHandler, WorkerError};
pub use protocol::{
    GrpcWorkerSession, WorkerSession, WorkerTaskStream, validate_activity_handlers,
};
