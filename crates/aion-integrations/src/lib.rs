//! Harness-integration SDK for Aion — the inbound analogue of `aion-client`.
//!
//! Where [`aion-client`](https://docs.rs/aion-client) is the SDK for a caller *driving* Aion,
//! `aion-integrations` is the SDK for an agent harness Aion *drives*. It defines the
//! harness-neutral extension seam an integrator implements — the [`AgentHarness`] /
//! [`AgentSession`] traits — plus the reusable machinery an adapter would otherwise hand-roll,
//! and it re-exports the neutral `aion-core` types so an integrator has a single dependency.
//!
//! # The seam
//!
//! Implement [`AgentHarness`] (in a separate adapter crate) to teach Aion how to run one agent
//! harness for one activity attempt. The seam is **harness-blind by construction**: no signature
//! names a concrete harness, a transport, or a wire protocol. A session:
//!
//! - advertises which neutral intervention primitives it supports via
//!   [`AgentSession::capabilities`] — an **empty set is first-class** (an observability-only
//!   harness supports no interventions),
//! - streams neutral [`ActivityEvent`]s OUT via [`AgentSession::events`],
//! - accepts neutral [`InterventionCommand`]s IN via [`AgentSession::intervene`], and
//! - yields a single terminal [`Payload`] via [`AgentSession::wait_result`].
//!
//! # Building blocks
//!
//! - [`jsonrpc`] — a generic JSON-RPC 2.0 over newline-delimited stdio helper (envelopes, id
//!   correlation, a single serializing writer) that **any** stdio-JSON-RPC adapter reuses. It is
//!   machinery, not a harness: it names no concrete harness and no method namespace.
//!
//! # Re-exported neutral types
//!
//! The neutral `aion-core` types the integration surface speaks are re-exported from the crate
//! root (a curated re-export, the `aion-client` house style — not a blanket `pub use aion_core`),
//! so an integrator depends only on `aion-integrations`.

/// The harness-integration seam: the [`AgentHarness`] / [`AgentSession`] traits.
pub mod contract;
/// The harness-neutral error taxonomy for the seam.
pub mod error;
/// A generic JSON-RPC 2.0 over newline-delimited stdio building block.
pub mod jsonrpc;
/// The neutral run identity handed to a harness at start.
pub mod spec;

pub use contract::{AgentHarness, AgentSession};
pub use error::HarnessError;
pub use spec::AgentRunSpec;

// Curated re-export of the neutral `aion-core` types the integration surface speaks, so an
// integrator has one dependency and never reaches directly into `aion-core` for these.
pub use aion_core::{
    ActivityEvent, ActivityEventKind, ApprovalDecision, ContentType, InjectPriority,
    InterventionCapabilities, InterventionCommand, InterventionKind, InterventionPrimitive,
    MessageRole, Payload, ProgressDetail, StopKind,
};
// The id types an `AgentRunSpec` needs, so the spec can be constructed without a second dependency.
pub use aion_core::{ActivityId, WorkflowId};
