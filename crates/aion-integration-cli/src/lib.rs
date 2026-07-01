//! A second, independent [`AgentHarness`] integration for Aion — the empirical proof that
//! `aion-integrations` is a real, neutral SDK, not a Norn wrapper (NOI-8, §9.1).
//!
//! `aion-integration-norn` is the FIRST adapter (a rich, bidirectional JSON-RPC harness). This
//! crate is a SECOND adapter of a **deliberately different shape**, so two implementations of one
//! trait exist across two crates — which is what makes the boundary an SDK rather than a one-off.
//! Both fit the neutral seam WITHOUT any change to `aion-core`, `aion-integrations`, the wire, the
//! server, or the worker: if a second working integration required touching those, the contract
//! would be "Norn-with-a-flag," not neutral.
//!
//! # The two shapes in this crate
//!
//! - [`CliHarness`] — the **observability-only** case (case (b) in the design). A plain-stdout CLI
//!   agent with **no control channel at all**: [`CliHarness::start`] spawns a line-oriented process
//!   and hands its stdout to a [`CliSession`] whose pump **demuxes interleaved stdout into neutral
//!   [`ActivityEvent`]s** ([`demux`], mostly [`aion_core::ActivityEventKind::Raw`], some mapped). It
//!   advertises an **empty [`InterventionCapabilities`] set**, so [`CliSession::intervene`] rejects
//!   every command and the server offers no controls for it. This is a first-class tier — an empty
//!   capability advertisement is valid, not degenerate.
//! - [`MockAgentHarness`] — the **interveneable** case. A deterministic in-crate harness that
//!   advertises `{inject_message, cancel}`, ACCEPTS those, and cleanly REJECTS the three primitives
//!   it does not advertise (notably `respond_to_approval`) with a capability-not-supported NACK. It
//!   exercises the OTHER branch of the [`AgentSession::intervene`] contract, and it is a genuine
//!   harness the worker driver runs live (not a stub) so the full worker apply path — control
//!   channel → [`AgentSession::intervene`] → accept/gate → ack — is exercised end-to-end.
//!
//! # Harness neutrality (the whole point)
//!
//! Neither shape depends on Norn or on any wire protocol beyond the neutral seam. Both speak only
//! [`AgentRunSpec`] in, neutral [`ActivityEvent`]s out, neutral [`InterventionCommand`]s in (gated
//! by the neutral [`InterventionCapabilities`]), and a neutral terminal [`Payload`]. There is **no
//! Norn here** — this crate exists precisely to prove that.
//!
//! [`AgentHarness`]: aion_integrations::AgentHarness
//! [`AgentSession`]: aion_integrations::AgentSession
//! [`CliHarness::start`]: aion_integrations::AgentHarness::start
//! [`CliSession::intervene`]: aion_integrations::AgentSession::intervene
//! [`AgentSession::intervene`]: aion_integrations::AgentSession::intervene
//! [`ActivityEvent`]: aion_core::ActivityEvent
//! [`InterventionCommand`]: aion_core::InterventionCommand
//! [`InterventionCapabilities`]: aion_core::InterventionCapabilities
//! [`AgentRunSpec`]: aion_integrations::AgentRunSpec
//! [`Payload`]: aion_core::Payload

/// The pure stdout demux — plain CLI stdout lines → neutral [`aion_core::ActivityEvent`]s.
pub mod demux;
/// The observability-only [`aion_integrations::AgentHarness`] over a plain-stdout CLI agent.
pub mod harness;
/// The interveneable mock harness advertising `{inject_message, cancel}`.
pub mod mock;
/// The live observability-only session implementing [`aion_integrations::AgentSession`].
pub mod session;

pub use harness::CliHarness;
pub use mock::{MockAgentHarness, MockAgentSession};
pub use session::CliSession;
