//! Rust caller SDK for the `aion-server` workflow-management API.
//!
//! The SDK connects to an Aion server over gRPC and exposes typed helpers for
//! starting, signaling, querying, canceling, listing, describing, and subscribing
//! to workflows.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use aion_client::{ClientAuth, ClientBuilder};
//!
//! let mut builder = ClientBuilder::new("http://127.0.0.1:50051")
//!     .with_namespace("default");
//! if let Ok(token) = std::env::var("AION_AUTH_TOKEN") {
//!     builder = builder.with_auth(ClientAuth::bearer(token));
//! }
//! let client = builder.build().await?;
//! let _shared = client.clone();
//! # Ok(())
//! # }
//! ```

/// Client builder, authentication, TLS options, and workflow operations.
pub mod client;
/// Client-side error taxonomy.
pub mod error;
/// Workflow-scoped operation handle.
pub mod handle;
/// Operation option and response models.
pub mod ops;
/// Typed conversion helpers between serde values and Aion payloads.
pub mod payload;
/// Event stream and subscription helpers.
pub mod stream;
/// Transport adapters (gRPC, WebSocket event streaming, embedded engine).
pub mod transport;

pub use client::{Client, ClientAuth, ClientBuilder, TlsOptions};
pub use error::ClientError;
pub use handle::WorkflowHandle;
pub use ops::{ListPage, StartOptions, WorkflowDescription};
pub use payload::{from_payload, to_payload};
pub use stream::{EventStream, ResumingEventStream, SubscribeTarget};
