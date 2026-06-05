//! Rust caller SDK for the aion-server workflow-management API.

pub mod client;
pub mod error;
pub mod handle;
pub mod ops;
pub mod payload;
pub mod stream;
pub mod transport;

pub use client::{Client, ClientAuth, ClientBuilder, TlsOptions};
pub use error::ClientError;
pub use handle::WorkflowHandle;
pub use ops::{ListPage, StartOptions, WorkflowDescription};
pub use payload::{from_payload, to_payload};
pub use stream::{EventStream, ResumingEventStream, SubscribeTarget};
