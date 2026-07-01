//! The harness-integration seam: the [`AgentHarness`] trait an integrator implements and the
//! live [`AgentSession`] it produces.
//!
//! This is the SDK's published extension seam, mirroring `aion-client`'s `WorkflowTransport`
//! (a dedicated `#[async_trait]` contract module). It is **harness-blind by construction**: no
//! signature names a concrete harness, a transport, or a wire protocol. A harness is integrated
//! by implementing these traits in a separate adapter crate; the seam speaks only neutral run
//! identity, the neutral [`ActivityEvent`] / [`InterventionCommand`] types, the neutral
//! [`InterventionCapabilities`] advertisement, and a neutral terminal [`Payload`].
//!
//! # Designed from two cases so it is provably general
//!
//! The seam is designed against two deliberately different integrations at once, so it cannot be
//! shaped by any one harness:
//!
//! - a full rich-path harness (a bidirectional channel, streamed events out, acknowledged
//!   commands in, a structured terminal result), and
//! - a plain-stdout, **observability-only** CLI agent (no command channel at all): it demuxes
//!   interleaved stdout into [`ActivityEvent`]s (mostly [`aion_core::ActivityEventKind::Raw`]),
//!   advertises an **empty** [`InterventionCapabilities`] set, and yields its final output as the
//!   result.
//!
//! The empty-capability case is what forces [`AgentSession::intervene`] to be
//! optional-by-capability: an empty advertisement is a valid, first-class tier, and a session
//! that advertises it rejects every command with [`HarnessError::CapabilityNotSupported`].

use aion_core::{ActivityEvent, InterventionCapabilities, InterventionCommand, Payload};
use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::error::HarnessError;
use crate::spec::AgentRunSpec;

/// How to run one agent harness — the SDK's published extension seam.
///
/// An integrator implements this (in a separate adapter crate) to teach Aion how to spawn or
/// connect their harness for a single activity attempt. It is harness-blind: [`Self::start`]
/// takes only the neutral [`AgentRunSpec`] and returns a live [`AgentSession`].
#[async_trait]
pub trait AgentHarness: Send + Sync {
    /// The live session type this harness produces for one attempt.
    type Session: AgentSession;

    /// Spawns or connects the harness for one activity attempt, negotiates capabilities, and
    /// returns a live session.
    ///
    /// `spec` carries the neutral run identity (`workflow_id`, `activity_id`, `attempt`) and the
    /// input [`Payload`] — never any harness-specific configuration.
    ///
    /// # Errors
    ///
    /// Returns a [`HarnessError`] when the harness cannot be spawned/connected or when the
    /// capability handshake fails ([`HarnessError::Transport`] / [`HarnessError::Protocol`]).
    async fn start(&self, spec: AgentRunSpec) -> Result<Self::Session, HarnessError>;
}

/// A live agent run for one activity attempt.
///
/// Produced by [`AgentHarness::start`]. Exposes the negotiated capability set, a stream of
/// neutral events OUT, a neutral command sink IN, and a single terminal result.
#[async_trait]
pub trait AgentSession: Send {
    /// The capability set negotiated at start.
    ///
    /// The server and ops console gate on THIS, never on harness identity. An **empty** set is a
    /// first-class advertisement — an observability-only harness supports no interventions.
    fn capabilities(&self) -> &InterventionCapabilities;

    /// The stream of neutral events produced by this run.
    ///
    /// Every item is an [`ActivityEvent`]; how the adapter derives them (mapping a structured
    /// notification, or demuxing interleaved stdout into mostly
    /// [`aion_core::ActivityEventKind::Raw`]) is an adapter-internal detail invisible here.
    fn events(&mut self) -> BoxStream<'static, ActivityEvent>;

    /// Delivers a neutral intervention command into the running agent.
    ///
    /// A session whose advertised [`Self::capabilities`] set does not contain the command's
    /// primitive rejects it with [`HarnessError::CapabilityNotSupported`]. An observability-only
    /// session (empty set) rejects **every** command this way; the server never routes one to it
    /// because the advertised set is empty.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::CapabilityNotSupported`] when the command's primitive is not
    /// advertised, [`HarnessError::StaleTarget`] when the command targets a superseded attempt,
    /// or a transport/protocol error when delivery fails.
    async fn intervene(&self, cmd: InterventionCommand) -> Result<(), HarnessError>;

    /// Awaits the single terminal result of the run.
    ///
    /// Consumes the session: exactly one terminal result is produced per attempt, and it is the
    /// replay-authoritative activity output the worker captures.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Harness`] when the run reported an application-level failure, or a
    /// transport/protocol error when the terminal result could not be received.
    async fn wait_result(self) -> Result<Payload, HarnessError>;
}
