//! The standalone deployable. Wraps the transport-agnostic engine with HTTP/gRPC APIs, WebSocket event streaming, the remote-worker protocol endpoint, multi-tenancy, and dashboard hosting.

pub mod api;
pub mod config;
pub mod dashboard;
pub mod error;
pub mod namespace;
pub mod state;
pub mod stream;
pub mod worker;
