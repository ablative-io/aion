//! Transport adapters used by the client.
//!
//! - [`contract`] — the [`WorkflowTransport`] seam every adapter implements.
//! - [`grpc`] — unary workflow-management RPCs over the AW-owned tonic
//!   service; event subscriptions delegate to the WebSocket adapter.
//! - [`ws`] — event streaming over the server's `/events/stream` WebSocket
//!   endpoint, speaking the cross-SDK subscription protocol (JSON
//!   `SubscriptionRequest` first frame, `StreamedEvent` frames,
//!   `{"error": <WireError>}` terminal frames, `resume_from_seq` cursor).
//! - [`embedded`] — an in-process [`aion::Engine`] adapter with the same
//!   resume/replay-splice semantics as the server.

/// Transport seam shared by every adapter.
pub mod contract;
mod convert;
#[cfg(feature = "embedded")]
/// In-process engine transport.
pub mod embedded;
/// gRPC transport over the AW-owned workflow service.
pub mod grpc;
/// WebSocket event-stream transport.
pub mod ws;

pub use contract::{SubscriptionAttempt, WorkflowTransport};
#[cfg(feature = "embedded")]
pub use embedded::EmbeddedWorkflowTransport;
pub use grpc::GrpcWorkflowTransport;
