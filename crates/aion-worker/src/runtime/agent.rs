//! The harness-blind trait driver: [`spawn_agent`] drives ANY [`AgentHarness`]
//! generically (NOI-4, §3A.1).
//!
//! This is a NEW additive spawn mode beside the worker's existing one-shot
//! `.output()` capture (the `run_norn_step` path in the norn-fan-worker example,
//! which stays working and unchanged). It is the single place the worker DRIVES
//! an agent session:
//!
//! - it pumps the session's neutral [`AgentSession::events`] stream OUT to a
//!   caller-supplied event sink (the worker's transcript delivery — an
//!   [`mpsc::UnboundedSender<ActivityEvent>`], the `event_sender` NOI-5 wires to
//!   liminal),
//! - it feeds neutral commands IN from a caller-supplied control source (the
//!   `control_receiver` NOI-6 wires from the server's intervention PUSH) to
//!   [`AgentSession::intervene`], and
//! - it correlates the single terminal [`AgentSession::wait_result`] into
//!   [`DispatchOutcome::Completed { output }`][DispatchOutcome::Completed] — the
//!   same replay-authoritative output the one-shot capture produces today.
//!
//! **The worker stays harness-blind.** [`spawn_agent`] is generic over
//! `AgentHarness`; it never names a concrete adapter, a transport, or a wire
//! protocol. The concrete harness (Norn, or a future one) is injected by the
//! binary composition root (aion-cli), never chosen here. This crate depends on
//! `aion-integrations` for the TRAIT ONLY and has NO edge to
//! `aion-integration-norn` or `norn` (the §3A.4 invariant, CI-gated).
//!
//! # Result/event split (structural, §4.1)
//!
//! Events flow only to the event sink; the terminal result flows only from
//! [`AgentSession::wait_result`]. An event can never be captured as the result
//! and the result is never delivered as an event — the two are distinct channels
//! of the session by construction, so the driver never has to disambiguate them.

use aion_core::{InterventionCommand, InterventionOutcome};
use aion_integrations::contract::{AgentHarness, AgentSession, DynAgentHarness, DynAgentSession};
use aion_integrations::error::HarnessError;
use aion_integrations::spec::AgentRunSpec;
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::activity::ActivityFailure;
use crate::runtime::loop_::DispatchOutcome;

/// The neutral event sink the driver pumps a session's [`AgentSession::events`]
/// out to. This is the `event_sender` the worker's [`ActivityContext`] transcript
/// delivery installs; NOI-5 forwards it onto a liminal events channel.
///
/// [`ActivityContext`]: crate::context::ActivityContext
pub type ActivityEventSender = mpsc::UnboundedSender<aion_core::ActivityEvent>;

/// One routed intervention on the driver's control channel: the neutral command
/// plus an OPTIONAL reply channel the driver answers with the neutral
/// [`InterventionOutcome`] ack.
///
/// The ack is what closes the loop back to the operator (NOI-6 §6.4): after the
/// driver calls [`AgentSession::intervene`] it maps the session's result onto an
/// [`InterventionOutcome`] and, when an `ack` sender is present, replies with it.
/// A `None` ack is the fire-and-forget shape (used where no operator is waiting,
/// e.g. an internal test that only observes the applied side-effect).
#[derive(Debug)]
pub struct ControlMessage {
    /// The neutral command to apply to the session.
    pub command: InterventionCommand,
    /// Optional reply channel the driver answers with the applied/gated/stale ack.
    pub ack: Option<oneshot::Sender<InterventionOutcome>>,
}

impl ControlMessage {
    /// A fire-and-forget control message with no ack reply channel.
    #[must_use]
    pub const fn new(command: InterventionCommand) -> Self {
        Self { command, ack: None }
    }

    /// A control message paired with a reply channel for its ack.
    #[must_use]
    pub const fn with_ack(
        command: InterventionCommand,
        ack: oneshot::Sender<InterventionOutcome>,
    ) -> Self {
        Self {
            command,
            ack: Some(ack),
        }
    }
}

/// The neutral command source the driver feeds into a session's
/// [`AgentSession::intervene`]. This is the `control_receiver` the worker installs
/// per attempt; NOI-6 delivers server-routed operator commands (each with its ack
/// reply channel) onto it.
pub type ControlReceiver = mpsc::UnboundedReceiver<ControlMessage>;

/// Drives one activity attempt through the neutral [`AgentHarness`] seam and
/// returns its terminal [`DispatchOutcome`].
///
/// This is the harness-blind trait driver (NOI-4). It:
///
/// 1. starts the harness for `spec` (spawn/connect + capability handshake),
/// 2. concurrently pumps the session's [`AgentSession::events`] to `event_sender`
///    and feeds `control_receiver` commands to [`AgentSession::intervene`] until
///    the event stream ends, then
/// 3. awaits [`AgentSession::wait_result`] and maps it into
///    [`DispatchOutcome::Completed { output }`][DispatchOutcome::Completed].
///
/// A `control_receiver` of `None` runs the session with no intervention channel —
/// the observability-only shape. When present, a command whose primitive the
/// session does not advertise is rejected by the session with
/// [`HarnessError::CapabilityNotSupported`]; the driver logs that rejection and
/// keeps running (a gated command is a normal, non-fatal outcome, not a run
/// failure).
///
/// The event stream ending signals end-of-run: the driver then takes the terminal
/// result. This is why events must be a distinct channel from the result — the
/// driver relies on the stream closing (not on inspecting any event) to know the
/// run is done, and the result arrives only from [`AgentSession::wait_result`].
///
/// # Errors
///
/// Returns [`HarnessError`] when the harness cannot be started or when the
/// terminal result cannot be received. A harness-reported application failure
/// ([`HarnessError::Harness`]) is returned to the caller, which maps it to a
/// [`DispatchOutcome::Failed`] via [`harness_error_to_outcome`]; transport and
/// protocol faults are surfaced as-is for the caller to classify.
pub async fn spawn_agent<H>(
    harness: &H,
    spec: AgentRunSpec,
    event_sender: ActivityEventSender,
    control_receiver: Option<ControlReceiver>,
) -> Result<DispatchOutcome, HarnessError>
where
    H: AgentHarness,
{
    let mut session = harness.start(spec).await?;

    // The events stream is a detached `'static` stream: taking it does NOT borrow
    // the session, so the driver can still call `intervene`/`wait_result` on the
    // session while pumping events. This is what lets all three run in one task.
    // (Disambiguated to the typed `AgentSession` — the blanket `DynAgentSession`
    // impl also defines an `events`, so the method must name its trait.)
    let mut events = AgentSession::events(&mut session);
    let mut control = control_receiver;

    // Pump events out and commands in until the event stream closes (end-of-run).
    // Commands after the stream closes cannot be delivered — the session is
    // terminating — so the loop ends with the stream.
    loop {
        tokio::select! {
            biased;
            // A command from the server-routed control channel. Feed it into the
            // session; a capability-gated rejection is logged, not fatal.
            maybe_message = recv_control(&mut control) => {
                match maybe_message {
                    Some(message) => deliver_command(&session, message).await,
                    // The control channel closed: drop it and keep pumping events
                    // to the terminal result (a closed control channel is not an
                    // end-of-run signal — only the event stream closing is).
                    None => control = None,
                }
            }
            event = events.next() => {
                match event {
                    Some(event) => {
                        // A closed event sink means the transcript consumer went
                        // away; stop forwarding but keep draining to the result.
                        if event_sender.send(event).is_err() {
                            debug!("agent driver: event sink closed; stopping event forwarding");
                            break;
                        }
                    }
                    // End of the event stream == end of run. Take the result next.
                    None => break,
                }
            }
        }
    }

    // The single terminal result — the replay-authoritative activity output.
    let output = AgentSession::wait_result(session).await?;
    Ok(DispatchOutcome::Completed { output })
}

/// Drives one activity attempt through an ERASED [`DynAgentHarness`] — the same
/// harness-blind driver as [`spawn_agent`], but over a `dyn` harness a worker can
/// HOLD without being generic (the typed [`AgentHarness`] is not object-safe).
///
/// Behaviour is identical to [`spawn_agent`]: it starts the harness, concurrently
/// pumps the session's events to `event_sender` and feeds `control_receiver`
/// commands to the session, and maps the terminal result into
/// [`DispatchOutcome::Completed`]. The only difference is the erased session type,
/// so the whole event/intervention contract (§4.1 result/event split, capability
/// gating, stale-target no-op) holds unchanged.
///
/// # Errors
///
/// Returns [`HarnessError`] when the harness cannot be started or the terminal
/// result cannot be received — same taxonomy as [`spawn_agent`].
pub async fn spawn_dyn_agent(
    harness: &dyn DynAgentHarness,
    spec: AgentRunSpec,
    event_sender: ActivityEventSender,
    control_receiver: Option<ControlReceiver>,
) -> Result<DispatchOutcome, HarnessError> {
    let mut session = harness.start_dyn(spec).await?;
    let mut events = session.events();
    let mut control = control_receiver;

    loop {
        tokio::select! {
            biased;
            maybe_message = recv_control(&mut control) => {
                match maybe_message {
                    Some(message) => deliver_dyn_command(session.as_ref(), message).await,
                    None => control = None,
                }
            }
            event = events.next() => {
                match event {
                    Some(event) => {
                        if event_sender.send(event).is_err() {
                            debug!("agent driver: event sink closed; stopping event forwarding");
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    let output = session.wait_result().await?;
    Ok(DispatchOutcome::Completed { output })
}

/// The erased twin of [`deliver_command`] — delivers one command to a
/// [`DynAgentSession`], replies the ack, and logs a gated/stale outcome without
/// ending the run.
async fn deliver_dyn_command(session: &dyn DynAgentSession, message: ControlMessage) {
    let ControlMessage { command, ack } = message;
    let primitive = command.kind.primitive();
    let outcome = match session.intervene(command).await {
        Ok(()) => InterventionOutcome::Applied,
        Err(HarnessError::CapabilityNotSupported { .. }) => {
            InterventionOutcome::capability_not_supported(primitive)
        }
        Err(HarnessError::StaleTarget { detail }) => InterventionOutcome::stale_target(detail),
        Err(error) => InterventionOutcome::stale_target(error.to_string()),
    };
    match &outcome {
        InterventionOutcome::Applied => debug!(?primitive, "agent driver: intervention delivered"),
        InterventionOutcome::CapabilityNotSupported { primitive } => {
            debug!(
                ?primitive,
                "agent driver: intervention gated (capability not supported)"
            );
        }
        InterventionOutcome::StaleTarget { detail } => {
            warn!(?primitive, %detail, "agent driver: intervention delivery failed");
        }
    }
    if let Some(ack) = ack {
        drop(ack.send(outcome));
    }
}

/// Receives the next control message, or pends forever when there is no control
/// channel — so the `select!` arm simply never fires in the no-intervention case.
async fn recv_control(control: &mut Option<ControlReceiver>) -> Option<ControlMessage> {
    match control {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// Delivers one command to the session, replies its neutral ack (when a reply
/// channel is present), and logs a capability-gated rejection or a delivery fault
/// without ending the run.
async fn deliver_command<S>(session: &S, message: ControlMessage)
where
    S: AgentSession,
{
    let ControlMessage { command, ack } = message;
    let primitive = command.kind.primitive();
    let outcome = apply_command(session, command).await;
    match &outcome {
        InterventionOutcome::Applied => {
            debug!(?primitive, "agent driver: intervention delivered");
        }
        InterventionOutcome::CapabilityNotSupported { primitive } => {
            // A gated command is a normal outcome of capability negotiation, not a
            // run failure: the server should not route an unadvertised primitive,
            // but if one arrives the driver rejects it cleanly and keeps running.
            debug!(
                ?primitive,
                "agent driver: intervention gated (capability not supported)"
            );
        }
        InterventionOutcome::StaleTarget { detail } => {
            // A transport/protocol/stale fault delivering a command does not fail
            // the run — the run continues and its terminal result stands.
            warn!(?primitive, %detail, "agent driver: intervention delivery failed");
        }
    }
    // Reply the ack to the waiting operator, if any. A dropped receiver (operator
    // gone) is benign — the command still applied to the session.
    if let Some(ack) = ack {
        drop(ack.send(outcome));
    }
}

/// Applies one command to the session and maps the session's result onto the
/// neutral [`InterventionOutcome`] ack the operator receives.
///
/// The mapping is the harness-blind translation of the neutral error taxonomy into
/// the three locked outcome classes (§6.4): a capability-gated rejection becomes
/// [`InterventionOutcome::CapabilityNotSupported`]; a stale-target rejection or any
/// transport/protocol/harness fault becomes [`InterventionOutcome::StaleTarget`]
/// (an honest NACK, never a crash — the run continues regardless).
async fn apply_command<S>(session: &S, command: InterventionCommand) -> InterventionOutcome
where
    S: AgentSession,
{
    let primitive = command.kind.primitive();
    match session.intervene(command).await {
        Ok(()) => InterventionOutcome::Applied,
        Err(HarnessError::CapabilityNotSupported { .. }) => {
            InterventionOutcome::capability_not_supported(primitive)
        }
        Err(HarnessError::StaleTarget { detail }) => InterventionOutcome::stale_target(detail),
        Err(error) => InterventionOutcome::stale_target(error.to_string()),
    }
}

/// Maps a [`HarnessError`] into a [`DispatchOutcome::Failed`] a caller can report.
///
/// A [`HarnessError::Harness`] (the agent ran but reported failure) and a
/// transport/protocol/stale fault are all retryable activity failures: the
/// attempt did not produce an accepted result, so the engine re-dispatches. This
/// keeps the trait driver's failure mapping in one place beside the driver.
#[must_use]
pub fn harness_error_to_outcome(error: &HarnessError) -> DispatchOutcome {
    DispatchOutcome::Failed {
        failure: ActivityFailure::retryable(error.to_string()).into(),
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
