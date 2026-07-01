//! The `InterventionCommand` type — the harness-neutral mid-run control vocabulary.
//!
//! This module defines the *typed contract* for the live, best-effort, mid-run control channel
//! INTO a running agent. It is the sibling of [`crate::activity_event`] (events flow OUT;
//! interventions flow IN) and, like it, a **non-replay real-time DTO** that crosses the
//! Rust -> TypeScript boundary via `ts-rs` into the ops-console generated bindings.
//!
//! # Harness neutrality (LOCKED)
//!
//! The command vocabulary is defined in **harness-neutral semantic primitives**, never in any
//! harness's native terms. There is no `Norn`, no `Steer`, no `Update`, no `CancellationToken`,
//! and no JSON-RPC concept anywhere in this module. The wire, the server, and the ops console
//! speak ONLY these neutral primitives; ALL harness-specific translation lives in exactly one
//! place — the worker-side per-harness adapter — never in this module.
//!
//! **The design test:** a primitive belongs in the neutral enum ONLY if it can plausibly map
//! onto a non-specific conversational-agent harness. Anything that only makes sense as one
//! harness's feature belongs behind the adapter, not here.
//!
//! # Capability gating and the empty set
//!
//! A harness advertises **which** neutral primitives it supports via [`InterventionCapabilities`].
//! An **empty** capability set is first-class and valid, not an error: an observability-only
//! harness advertises no primitives, and the ops console offers no controls for it. The server
//! gates every command against the advertised set before it is ever routed.
//!
//! # Observability, never replay
//!
//! An intervention is recorded as a durable observability event (so the transcript shows
//! "operator intervened here") but is **never** part of the workflow replay log. In particular
//! [`InterventionKind::Cancel`] stops the *agent run* as a control act; it does NOT write
//! workflow replay state — a workflow-visible cancel/signal is a different thing entirely and
//! stays on the engine's replay-log paths. These types carry no behaviour — they are pure data.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ActivityId, WorkflowId};

/// Priority of an injected out-of-band message.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "priority")]
pub enum InjectPriority {
    /// A queued user turn — batches, and may not wake an idle agent.
    Normal,
    /// Act now. This is what "steer" is: an interrupt-priority injection.
    Interrupt,
}

/// A decision answering a pending human-in-the-loop approval gate.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "decision")]
pub enum ApprovalDecision {
    /// Allow the agent's proposed next action to proceed.
    Approve,
    /// Decline the agent's proposed next action.
    Deny,
}

/// The complete set of harness-neutral mid-run control primitives.
///
/// Exactly five primitives — the whole universal agent-control surface. Each is gated by the
/// harness's advertised [`InterventionCapabilities`]. None is specific to any one harness:
/// [`Self::InjectMessage`] and [`Self::Cancel`] are universal; [`Self::PauseResume`] is the
/// standard suspend/resume any stepped agent loop can expose; [`Self::UpdateBudget`] maps onto
/// any harness with token/turn limits; [`Self::RespondToApproval`] maps onto any harness with a
/// tool-use / permission gate.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum InterventionKind {
    /// Inject an out-of-band user turn into the running agent (steer / redirect / add context).
    ///
    /// SUBSUMES "steer": steering is just an [`InjectPriority::Interrupt`] injection. There is no
    /// separate `Steer`/`Update` variant in the neutral enum.
    InjectMessage {
        /// The message text to inject.
        text: String,
        /// Whether to act now (`Interrupt`) or queue the turn (`Normal`).
        priority: InjectPriority,
    },
    /// Stop the agent run (this run's current execution).
    ///
    /// This is an observability/control act and is DISTINCT from a workflow-visible
    /// cancel/signal, which stays on the engine's replay-log paths and is NOT an intervention.
    Cancel {
        /// Human-readable reason for the cancellation.
        reason: String,
    },
    /// Suspend or resume the agent between steps.
    ///
    /// Capability-gated: harnesses that cannot suspend mid-step advertise no support for it.
    PauseResume {
        /// `true` to suspend, `false` to resume.
        paused: bool,
    },
    /// Adjust the run's resource limits mid-flight.
    UpdateBudget {
        /// New maximum token budget, when the operator sets one.
        max_tokens: Option<u64>,
        /// New maximum turn budget, when the operator sets one.
        max_turns: Option<u32>,
    },
    /// Answer a pending tool-use / permission gate — human-in-the-loop approval of the agent's
    /// next action.
    RespondToApproval {
        /// Correlation id of the pending approval being answered.
        call_id: String,
        /// The approve/deny decision.
        decision: ApprovalDecision,
        /// An optional note recorded alongside the decision.
        note: Option<String>,
    },
}

/// A mid-run control command routed operator -> server -> the worker owning the activity-attempt.
///
/// Recorded as a durable observability event (auditable, visible on transcript replay) but
/// **never** part of the workflow replay log. A command addressed to a stale attempt is a no-op.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
pub struct InterventionCommand {
    /// The workflow the target activity belongs to.
    pub workflow_id: WorkflowId,
    /// The target activity within the workflow.
    pub activity_id: ActivityId,
    /// The target attempt. Commands to a stale (superseded) attempt are no-ops.
    pub attempt: u32,
    /// The auth subject that issued the command, when auth is enabled.
    ///
    /// A neutral subject label (the same identity the auth layer records), carried so the
    /// transcript can attribute the intervention. `None` when auth is off.
    pub issued_by: Option<String>,
    /// When the command was issued (operator-clock instant).
    pub issued_at: DateTime<Utc>,
    /// The neutral control primitive to apply.
    pub kind: InterventionKind,
}

/// The neutral outcome of routing one [`InterventionCommand`] to the worker owning the target
/// attempt — the ack that surfaces back to the operator.
///
/// The three variants ARE the three distinct outcome classes the design locks (§6.4), expressed
/// harness-neutrally so the wire, the server, and the ops console never inspect a harness error:
///
/// - [`Self::Applied`] — the session accepted and applied the command.
/// - [`Self::CapabilityNotSupported`] — the target harness does not advertise the command's
///   primitive. The server gates on the advertised set BEFORE routing, so this is normally
///   returned by the server without a wire round-trip; a worker returns it too if a gated command
///   still reaches it.
/// - [`Self::StaleTarget`] — the target `(workflow, activity, attempt)` is finished, superseded by
///   a later attempt, or unknown (the attempt-scoped no-op). It is an honest NACK, never a crash.
///
/// Carried as its own enum (not a `Result`) so it round-trips over `ts-rs` into the ops console
/// exactly like the other real-time DTOs, and so a future outcome class is an additive variant.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "outcome")]
pub enum InterventionOutcome {
    /// The command was delivered to the live session and applied.
    Applied,
    /// The command's primitive is not in the target harness's advertised capability set.
    CapabilityNotSupported {
        /// The primitive the target does not support.
        primitive: InterventionPrimitive,
    },
    /// The target attempt is finished, superseded, or unknown — an attempt-scoped no-op.
    StaleTarget {
        /// Human-readable detail describing why the target is stale.
        detail: String,
    },
}

impl InterventionOutcome {
    /// Returns `true` when the command was applied to a live session.
    #[must_use]
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied)
    }

    /// Builds a [`Self::CapabilityNotSupported`] naming the ungated primitive.
    #[must_use]
    pub const fn capability_not_supported(primitive: InterventionPrimitive) -> Self {
        Self::CapabilityNotSupported { primitive }
    }

    /// Builds a [`Self::StaleTarget`] with a detail message.
    #[must_use]
    pub fn stale_target(detail: impl Into<String>) -> Self {
        Self::StaleTarget {
            detail: detail.into(),
        }
    }
}

/// A single neutral intervention primitive, independent of any command payload.
///
/// The discriminant an [`InterventionCapabilities`] advertises and the primitive each
/// [`InterventionKind`] belongs to. Modelled as its own enum (rather than five booleans) so the
/// capability set is an explicit set of primitives with no fixed-width shape to grow.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(tag = "primitive")]
pub enum InterventionPrimitive {
    /// Corresponds to [`InterventionKind::InjectMessage`].
    InjectMessage,
    /// Corresponds to [`InterventionKind::Cancel`].
    Cancel,
    /// Corresponds to [`InterventionKind::PauseResume`].
    PauseResume,
    /// Corresponds to [`InterventionKind::UpdateBudget`].
    UpdateBudget,
    /// Corresponds to [`InterventionKind::RespondToApproval`].
    RespondToApproval,
}

impl InterventionKind {
    /// The neutral primitive this command belongs to.
    #[must_use]
    pub const fn primitive(&self) -> InterventionPrimitive {
        match self {
            Self::InjectMessage { .. } => InterventionPrimitive::InjectMessage,
            Self::Cancel { .. } => InterventionPrimitive::Cancel,
            Self::PauseResume { .. } => InterventionPrimitive::PauseResume,
            Self::UpdateBudget { .. } => InterventionPrimitive::UpdateBudget,
            Self::RespondToApproval { .. } => InterventionPrimitive::RespondToApproval,
        }
    }
}

/// The set of neutral intervention primitives a harness advertises support for.
///
/// The server and ops console gate on THIS, never on harness identity. An **empty** set is a
/// first-class, valid advertisement — an observability-only harness supports no interventions,
/// and the console offers no controls for it. It is a legitimate tier, not a degenerate one.
///
/// Modelled as an explicit list of supported [`InterventionPrimitive`]s. Duplicates are ignored
/// by the accessors; ordering is not significant.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq, Default)]
pub struct InterventionCapabilities {
    /// The primitives this harness supports. Empty = observability-only.
    pub supported: Vec<InterventionPrimitive>,
}

impl InterventionCapabilities {
    /// The empty capability set — an observability-only harness that supports no interventions.
    ///
    /// This is a first-class, valid advertisement (not an error): the ops console offers no
    /// controls for a harness advertising it.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Builds a capability set from an iterator of supported primitives.
    pub fn from_primitives(primitives: impl IntoIterator<Item = InterventionPrimitive>) -> Self {
        Self {
            supported: primitives.into_iter().collect(),
        }
    }

    /// Returns `true` when no intervention primitive is supported (observability-only).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.supported.is_empty()
    }

    /// Returns `true` when the given primitive is advertised as supported.
    #[must_use]
    pub fn supports_primitive(&self, primitive: InterventionPrimitive) -> bool {
        self.supported.contains(&primitive)
    }

    /// Returns `true` when the given command's primitive is advertised as supported.
    ///
    /// The server uses this to refuse an unadvertised primitive before it is ever routed.
    #[must_use]
    pub fn supports(&self, kind: &InterventionKind) -> bool {
        self.supports_primitive(kind.primitive())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde::de::DeserializeOwned;

    use super::{
        ApprovalDecision, InjectPriority, InterventionCapabilities, InterventionCommand,
        InterventionKind, InterventionOutcome, InterventionPrimitive, WorkflowId,
    };
    use crate::ids::ActivityId;

    fn fixed_time() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default()
    }

    fn round_trip<T>(value: &T) -> Result<T, serde_json::Error>
    where
        T: DeserializeOwned + serde::Serialize,
    {
        let json = serde_json::to_string(value)?;
        serde_json::from_str::<T>(&json)
    }

    fn command(kind: InterventionKind) -> InterventionCommand {
        InterventionCommand {
            workflow_id: WorkflowId::new(uuid::Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(3),
            attempt: 1,
            issued_by: Some("operator@example.com".to_owned()),
            issued_at: fixed_time(),
            kind,
        }
    }

    #[test]
    fn every_intervention_variant_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let kinds = vec![
            InterventionKind::InjectMessage {
                text: "use the other module".to_owned(),
                priority: InjectPriority::Interrupt,
            },
            InterventionKind::InjectMessage {
                text: "some context".to_owned(),
                priority: InjectPriority::Normal,
            },
            InterventionKind::Cancel {
                reason: "operator abort".to_owned(),
            },
            InterventionKind::PauseResume { paused: true },
            InterventionKind::PauseResume { paused: false },
            InterventionKind::UpdateBudget {
                max_tokens: Some(10_000),
                max_turns: None,
            },
            InterventionKind::RespondToApproval {
                call_id: "call-9".to_owned(),
                decision: ApprovalDecision::Approve,
                note: Some("looks fine".to_owned()),
            },
            InterventionKind::RespondToApproval {
                call_id: "call-10".to_owned(),
                decision: ApprovalDecision::Deny,
                note: None,
            },
        ];
        for kind in kinds {
            let cmd = command(kind);
            let decoded = round_trip(&cmd)?;
            assert_eq!(cmd, decoded);
        }
        Ok(())
    }

    #[test]
    fn command_without_auth_subject_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let mut cmd = command(InterventionKind::Cancel {
            reason: "shutdown".to_owned(),
        });
        cmd.issued_by = None;
        let decoded = round_trip(&cmd)?;
        assert_eq!(decoded.issued_by, None);
        assert_eq!(cmd, decoded);
        Ok(())
    }

    #[test]
    fn empty_capability_set_is_first_class() -> Result<(), Box<dyn std::error::Error>> {
        let observability_only = InterventionCapabilities::none();
        assert!(observability_only.is_empty());
        assert_eq!(observability_only, InterventionCapabilities::default());

        // An empty set supports no primitive: the console offers no controls, and the server
        // refuses every command before routing it. This is a valid tier, not an error.
        let cancel = InterventionKind::Cancel {
            reason: "x".to_owned(),
        };
        assert!(!observability_only.supports(&cancel));

        // Round-trips cleanly as a valid advertisement.
        let decoded = round_trip(&observability_only)?;
        assert_eq!(observability_only, decoded);
        Ok(())
    }

    #[test]
    fn capabilities_gate_on_advertised_primitives() {
        let caps = InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ]);
        assert!(!caps.is_empty());
        assert!(caps.supports(&InterventionKind::InjectMessage {
            text: "hi".to_owned(),
            priority: InjectPriority::Normal,
        }));
        assert!(caps.supports(&InterventionKind::Cancel {
            reason: "stop".to_owned(),
        }));
        assert!(!caps.supports(&InterventionKind::PauseResume { paused: true }));
        assert!(!caps.supports(&InterventionKind::UpdateBudget {
            max_tokens: None,
            max_turns: None,
        }));
    }

    #[test]
    fn every_outcome_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let outcomes = vec![
            InterventionOutcome::Applied,
            InterventionOutcome::capability_not_supported(InterventionPrimitive::PauseResume),
            InterventionOutcome::stale_target("attempt 2 superseded"),
        ];
        for outcome in outcomes {
            let decoded = round_trip(&outcome)?;
            assert_eq!(outcome, decoded);
        }
        // Only `Applied` reports applied; the two NACK classes do not.
        assert!(InterventionOutcome::Applied.is_applied());
        assert!(!InterventionOutcome::stale_target("gone").is_applied());
        assert!(
            !InterventionOutcome::capability_not_supported(InterventionPrimitive::Cancel)
                .is_applied()
        );
        Ok(())
    }
}
