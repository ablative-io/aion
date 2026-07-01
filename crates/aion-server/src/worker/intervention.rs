//! Server-side mid-run intervention routing (NOI-6).
//!
//! This is the SERVER half of the intervention path: it takes a neutral
//! [`InterventionCommand`] an operator submitted, resolves the worker currently
//! owning the target `(workflow, activity, attempt)` session, **gates on that
//! worker's advertised [`InterventionCapabilities`]**, and — only for an advertised
//! primitive — routes the command to the owning worker over a pluggable
//! [`InterventionTransport`] (the production one is the liminal server-push, §6.2).
//! It returns the neutral [`InterventionOutcome`] ack the operator sees.
//!
//! # Harness-neutral by construction
//!
//! Nothing here names a harness or a wire protocol. The router speaks ONLY neutral
//! `aion-core` types; the transport trait carries a neutral command and returns a
//! neutral ack, so the only harness-specific translation stays behind the worker's
//! `AgentSession`, far below this module.
//!
//! # The three locked outcome classes (§6.4)
//!
//! - **Not supported** — the owning worker does not advertise the command's
//!   primitive. The router refuses it at the SERVER and NEVER sends it, returning
//!   [`InterventionOutcome::capability_not_supported`]. This is the LOCKED
//!   server-side gate: `-32601` is reserved for the degenerate protocol bug of a
//!   child rejecting a method the server should have gated, NEVER routine gating.
//! - **Too late / wrong attempt** — no worker owns the target attempt (finished,
//!   superseded, or unknown). The router returns
//!   [`InterventionOutcome::stale_target`] — the attempt-scoped no-op, an honest
//!   NACK surfaced to the operator, never a crash.
//! - **Applied** — the command reached the live session and was applied.

use aion_core::{
    ActivityId, InterventionCapabilities, InterventionCommand, InterventionOutcome, WorkflowId,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::registry::{ConnectedWorkerRegistry, WorkerHandle, WorkerId};
use crate::error::ServerError;

/// The `(workflow, activity, attempt)` key one running agent session is addressed
/// by — the same identity the worker back-index and the whole design key on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AttemptKey {
    /// The workflow the target activity belongs to.
    pub workflow_id: WorkflowId,
    /// The target activity within the workflow.
    pub activity_id: ActivityId,
    /// The target attempt. A command to any other attempt is a stale-target no-op.
    pub attempt: u32,
}

impl AttemptKey {
    /// Build an attempt key from its three components.
    #[must_use]
    pub const fn new(workflow_id: WorkflowId, activity_id: ActivityId, attempt: u32) -> Self {
        Self {
            workflow_id,
            activity_id,
            attempt,
        }
    }

    /// The attempt key naming the target of a routed command.
    #[must_use]
    pub fn of_command(command: &InterventionCommand) -> Self {
        Self::new(
            command.workflow_id.clone(),
            command.activity_id.clone(),
            command.attempt,
        )
    }
}

/// Server-side `attempt -> owning-worker` back-index (§6.2).
///
/// The router resolves the CURRENT owner of a target attempt here. An entry is
/// installed when an agent attempt is dispatched to a worker and removed when it
/// completes / fails over, so the index reflects who owns each live attempt right
/// now — NOT a stale registry snapshot. A miss is the attempt-scoped no-op.
#[derive(Clone, Debug, Default)]
pub struct AttemptOwnerIndex {
    inner: Arc<Mutex<HashMap<AttemptKey, WorkerId>>>,
}

impl AttemptOwnerIndex {
    /// Build an empty owner index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `worker` currently owns the session for `key`.
    ///
    /// Called when an agent attempt is dispatched. A later attempt overwrites the
    /// prior entry for the same `(workflow, activity)` at a new attempt number —
    /// each attempt is its own key, so a superseded attempt simply has no entry.
    pub fn bind(&self, key: AttemptKey, worker: WorkerId) {
        if let Ok(mut index) = self.inner.lock() {
            index.insert(key, worker);
        }
    }

    /// Remove the owner entry for `key` (the attempt finished or migrated).
    pub fn release(&self, key: &AttemptKey) {
        if let Ok(mut index) = self.inner.lock() {
            index.remove(key);
        }
    }

    /// The worker currently owning the session for `key`, if any.
    #[must_use]
    pub fn owner(&self, key: &AttemptKey) -> Option<WorkerId> {
        self.inner.lock().ok()?.get(key).copied()
    }

    /// Every live attempt owned for `workflow_id`, paired with its owning worker.
    ///
    /// The console reads this to enumerate the attempts an operator can currently
    /// intervene on within one workflow — only attempts with a LIVE owner appear,
    /// so a finished or superseded attempt (which has no entry) is never offered.
    /// A poisoned lock yields an empty enumeration, never a panic.
    #[must_use]
    pub fn attempts_for_workflow(&self, workflow_id: &WorkflowId) -> Vec<(AttemptKey, WorkerId)> {
        let Ok(index) = self.inner.lock() else {
            return Vec::new();
        };
        index
            .iter()
            .filter(|(key, _worker)| &key.workflow_id == workflow_id)
            .map(|(key, worker)| (key.clone(), *worker))
            .collect()
    }
}

/// The transport the router pushes a gated command to the owning worker over.
///
/// The production implementation is the liminal server-push
/// ([`LiminalInterventionTransport`], §6.2); a test/in-proc implementation delivers
/// straight to a worker's control back-index. The trait is neutral: a command in,
/// a neutral ack out.
#[async_trait]
pub trait InterventionTransport: Send + Sync {
    /// Push one command to the worker addressed by `worker` and return its ack.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] only for a genuine transport fault (the connection
    /// was lost before the command could be delivered). A capability-gated or
    /// stale-target *outcome* is a normal [`InterventionOutcome`], not an error.
    async fn push(
        &self,
        worker: &WorkerHandle,
        command: InterventionCommand,
    ) -> Result<InterventionOutcome, ServerError>;
}

/// Routes an operator's neutral command to the worker owning the target attempt,
/// gating on the worker's advertised capabilities first (NOI-6).
pub struct InterventionRouter {
    registry: ConnectedWorkerRegistry,
    owners: AttemptOwnerIndex,
    transport: Arc<dyn InterventionTransport>,
}

impl std::fmt::Debug for InterventionRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InterventionRouter").finish_non_exhaustive()
    }
}

impl InterventionRouter {
    /// Build a router over the connected-worker `registry`, the attempt-owner
    /// `owners` index, and the command `transport`.
    #[must_use]
    pub fn new(
        registry: ConnectedWorkerRegistry,
        owners: AttemptOwnerIndex,
        transport: Arc<dyn InterventionTransport>,
    ) -> Self {
        Self {
            registry,
            owners,
            transport,
        }
    }

    /// The attempt-owner index the router resolves through, so the dispatch path
    /// can bind/release ownership on the same index.
    #[must_use]
    pub fn owners(&self) -> &AttemptOwnerIndex {
        &self.owners
    }

    /// Route one operator command to the owning worker, returning the neutral ack.
    ///
    /// Resolves the owning worker for the command's `(workflow, activity, attempt)`.
    /// A missing owner or a registry entry that has since disconnected is the
    /// attempt-scoped no-op ([`InterventionOutcome::stale_target`]). When the owning
    /// worker does not advertise the command's primitive, the router refuses it at
    /// the server ([`InterventionOutcome::capability_not_supported`]) and NEVER
    /// sends it. Otherwise it pushes over the transport and returns the worker's
    /// ack. A transport fault (the connection dropped mid-route) is mapped to a
    /// stale-target no-op — the target is unreachable, which is exactly the
    /// too-late class from the operator's view.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] only if the registry lock is poisoned.
    pub async fn route(
        &self,
        command: InterventionCommand,
    ) -> Result<InterventionOutcome, ServerError> {
        let key = AttemptKey::of_command(&command);
        let primitive = command.kind.primitive();

        let Some(worker_id) = self.owners.owner(&key) else {
            return Ok(stale(&key));
        };
        let Some(worker) = self.registry.worker_by_id(worker_id)? else {
            // The owner disconnected between binding and routing: too-late no-op.
            return Ok(stale(&key));
        };

        // The LOCKED server-side capability gate: refuse an unadvertised primitive
        // HERE and never emit it to the worker.
        if !worker.intervention_capabilities().supports(&command.kind) {
            return Ok(InterventionOutcome::capability_not_supported(primitive));
        }

        match self.transport.push(&worker, command).await {
            Ok(outcome) => Ok(outcome),
            // A dropped connection means the owning attempt is unreachable — from
            // the operator's view that is the too-late / gone class, an honest NACK.
            Err(error) if error.is_worker_connection_lost() => {
                Ok(InterventionOutcome::stale_target(format!(
                    "owning worker connection lost before the command was applied: {error}"
                )))
            }
            Err(error) => Err(error),
        }
    }

    /// The advertised capability set of the worker currently owning `key`, if any —
    /// what the ops console reads to decide which controls to offer (NOI-7).
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn capabilities_for(
        &self,
        key: &AttemptKey,
    ) -> Result<Option<InterventionCapabilities>, ServerError> {
        let Some(worker_id) = self.owners.owner(key) else {
            return Ok(None);
        };
        Ok(self
            .registry
            .worker_by_id(worker_id)?
            .map(|worker| worker.intervention_capabilities().clone()))
    }

    /// Every live, intervenable attempt of `workflow_id` paired with its owning
    /// worker's advertised [`InterventionCapabilities`] — the enumeration the ops
    /// console reads to pick a target and gate controls (NOI-7).
    ///
    /// Only attempts with a LIVE owner appear: a finished or superseded attempt has
    /// no owner entry and is not enumerated. An attempt whose owner has since
    /// disconnected (present in the index but gone from the registry) is likewise
    /// dropped, so the console never offers a control for an unreachable attempt.
    /// The capability set is the SAME advertised set the router gates `route` on, so
    /// the console and the server agree on exactly which primitives are supported.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the registry lock is poisoned.
    pub fn intervenable_attempts(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<(AttemptKey, InterventionCapabilities)>, ServerError> {
        let mut attempts = Vec::new();
        for (key, worker_id) in self.owners.attempts_for_workflow(workflow_id) {
            if let Some(worker) = self.registry.worker_by_id(worker_id)? {
                attempts.push((key, worker.intervention_capabilities().clone()));
            }
        }
        Ok(attempts)
    }
}

/// The stale-target no-op ack for a target with no live owner.
fn stale(key: &AttemptKey) -> InterventionOutcome {
    InterventionOutcome::stale_target(format!(
        "no live owner for attempt {} of activity {} in workflow {}",
        key.attempt, key.activity_id, key.workflow_id
    ))
}

#[cfg(test)]
#[path = "intervention_tests.rs"]
mod tests;
