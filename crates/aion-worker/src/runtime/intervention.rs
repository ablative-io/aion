//! Worker-side mid-run intervention delivery (NOI-6).
//!
//! This is the WORKER half of the intervention routing path: a command routed
//! operator -> server -> liminal PUSH arrives here and is delivered to the
//! in-flight agent session that owns the target `(workflow, activity, attempt)`,
//! which applies it via [`AgentSession::intervene`](aion_integrations::contract::AgentSession::intervene)
//! and returns a neutral ack.
//!
//! # Harness-neutral by construction
//!
//! Nothing in this module names a harness, a wire protocol, or a concrete adapter.
//! It speaks ONLY the neutral `aion-core` control vocabulary: an
//! [`InterventionCommand`] in, an [`InterventionOutcome`] ack out. The command is
//! delivered onto the session's driver control channel (a [`ControlMessage`]),
//! where [`spawn_agent`](super::agent::spawn_agent) drains it into the session and
//! replies the ack — so the harness-specific translation stays behind the
//! `AgentSession` trait, never here.
//!
//! # The attempt back-index (§6.4)
//!
//! [`ControlRegistry`] is the worker's `(workflow, activity, attempt) -> session
//! control channel` back-index. A command whose target has no live entry — because
//! the attempt finished, was superseded by a later attempt, or never ran here — is
//! the attempt-scoped no-op: it returns [`InterventionOutcome::stale_target`], an
//! honest NACK, never a panic. A command whose primitive the target session does
//! not advertise returns [`InterventionOutcome::capability_not_supported`]. Neither
//! is an error: they are two of the three locked outcome classes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aion_core::{
    ActivityId, InterventionCapabilities, InterventionCommand, InterventionOutcome, WorkflowId,
};
use tokio::sync::{mpsc, oneshot};

use super::agent::ControlMessage;

/// The `(workflow, activity, attempt)` key one running agent session is addressed
/// by — the exact identity the design keys the whole intervention path on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SessionKey {
    /// The workflow the target activity belongs to.
    pub workflow_id: WorkflowId,
    /// The target activity within the workflow.
    pub activity_id: ActivityId,
    /// The target attempt. A command to any other attempt is a stale-target no-op.
    pub attempt: u32,
}

impl SessionKey {
    /// Build a session key from its three components.
    #[must_use]
    pub const fn new(workflow_id: WorkflowId, activity_id: ActivityId, attempt: u32) -> Self {
        Self {
            workflow_id,
            activity_id,
            attempt,
        }
    }

    /// The session key naming the target of a routed command.
    #[must_use]
    pub fn of_command(command: &InterventionCommand) -> Self {
        Self::new(
            command.workflow_id.clone(),
            command.activity_id.clone(),
            command.attempt,
        )
    }
}

/// One live session's control leg: the driver control-channel sender the command
/// is delivered onto, plus the neutral capability set the worker gates on.
#[derive(Clone, Debug)]
struct SessionControl {
    control: mpsc::UnboundedSender<ControlMessage>,
    capabilities: InterventionCapabilities,
}

/// The worker's attempt back-index: maps a live [`SessionKey`] to its session
/// control leg, so a routed command reaches the exact in-flight attempt.
///
/// A session installs itself with [`Self::register`] when it starts and the
/// returned [`SessionGuard`] removes it on drop, so the index tracks exactly the
/// sessions running on this worker. It is cheap to clone (an `Arc` inside), so the
/// liminal serve loop and the session-spawn path share one index.
#[derive(Clone, Debug, Default)]
pub struct ControlRegistry {
    inner: Arc<Mutex<HashMap<SessionKey, SessionControl>>>,
}

impl ControlRegistry {
    /// Build an empty control registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a live session's control leg under its `key`, returning a guard
    /// whose drop deregisters it (so the index never routes to a gone session).
    ///
    /// The `control` sender is the driver's control channel (the receiver half is
    /// handed to [`spawn_agent`](super::agent::spawn_agent) as its
    /// `control_receiver`); `capabilities` is the session's advertised neutral
    /// primitive set, which the worker gates on before delivering a command.
    #[must_use]
    pub fn register(
        &self,
        key: SessionKey,
        control: mpsc::UnboundedSender<ControlMessage>,
        capabilities: InterventionCapabilities,
    ) -> SessionGuard {
        if let Ok(mut index) = self.inner.lock() {
            index.insert(
                key.clone(),
                SessionControl {
                    control,
                    capabilities,
                },
            );
        }
        SessionGuard {
            registry: self.clone(),
            key,
        }
    }

    /// Deliver one routed command to the session that owns its target, returning
    /// the neutral [`InterventionOutcome`] ack.
    ///
    /// Resolves the target session by `(workflow, activity, attempt)`. When no live
    /// session owns the target, returns [`InterventionOutcome::stale_target`] — the
    /// attempt-scoped no-op. When the session's advertised capabilities do not
    /// include the command's primitive, returns
    /// [`InterventionOutcome::capability_not_supported`] WITHOUT delivering it (the
    /// worker gate mirrors the server gate; a well-behaved server never sends an
    /// unadvertised primitive, but the worker refuses one cleanly if it arrives).
    /// Otherwise it delivers the command and awaits the driver's ack.
    pub async fn deliver(&self, command: InterventionCommand) -> InterventionOutcome {
        let key = SessionKey::of_command(&command);
        let primitive = command.kind.primitive();
        let Some(session) = self.lookup(&key) else {
            return InterventionOutcome::stale_target(format!(
                "no live session for attempt {} of activity {} in workflow {}",
                key.attempt, key.activity_id, key.workflow_id
            ));
        };
        if !session.capabilities.supports(&command.kind) {
            return InterventionOutcome::capability_not_supported(primitive);
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        if session
            .control
            .send(ControlMessage::with_ack(command, ack_tx))
            .is_err()
        {
            // The driver's control receiver is gone — the session ended between the
            // lookup and the send. This is the stale-target no-op, not a fault.
            return InterventionOutcome::stale_target(format!(
                "session for attempt {} ended before the command was applied",
                key.attempt
            ));
        }
        match ack_rx.await {
            Ok(outcome) => outcome,
            // The driver dropped the ack sender without replying (session ended
            // mid-apply): honest stale-target NACK, never a crash.
            Err(_) => InterventionOutcome::stale_target(format!(
                "session for attempt {} ended before acking the command",
                key.attempt
            )),
        }
    }

    /// Snapshot a session's control leg for `key`, if one is registered.
    fn lookup(&self, key: &SessionKey) -> Option<SessionControl> {
        self.inner.lock().ok()?.get(key).cloned()
    }

    fn remove(&self, key: &SessionKey) {
        if let Ok(mut index) = self.inner.lock() {
            index.remove(key);
        }
    }
}

/// Drop guard removing a session's control leg from the [`ControlRegistry`] when
/// the session ends, so the back-index tracks exactly the live sessions.
#[derive(Debug)]
pub struct SessionGuard {
    registry: ControlRegistry,
    key: SessionKey,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.key);
    }
}

#[cfg(test)]
#[path = "intervention_tests.rs"]
mod tests;
