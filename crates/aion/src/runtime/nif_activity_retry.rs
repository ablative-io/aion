//! SDK-declared per-activity retry policy, honored at the dispatch seam (#197).
//!
//! The Gleam SDK has always shipped the activity's retry policy to the engine
//! inside the dispatch `config` JSON (`gleam/aion_flow/src/aion/workflow/run.gleam`,
//! `retry_config`), where — until this module — it was parsed by nothing. This
//! module is the consuming half: it decodes that policy and supplies the
//! retryable-failure classification and backoff math the completion-task retry
//! loop (`nif_activity_dispatch::spawn_completion_task`) drives.
//!
//! Classification: the worker reports a STRUCTURED `ActivityErrorKind` on the
//! wire; the server seam collapses it into the SDK's prefixed reason
//! vocabulary (`retryable:<message>` / `terminal:<message>`, see
//! `PendingActivities::complete_activity` in `aion-server`). By the time a
//! failure reaches the [`crate::activity::bridge::ActivityDispatcher`] seam it
//! is that string, so the `retryable:` prefix here IS the wire's structured
//! kind, not a heuristic over free text.
//!
//! Absence semantics match the SDK contract exactly: an activity with no
//! `retry` decorator carries `"retry": null` and runs exactly once — there is
//! no engine-imposed default policy.

use std::time::Duration;

use aion_core::{ActivityId, Event};

use crate::runtime::nif_activity_dispatch::FIRST_DELIVERY_ATTEMPT;

/// Reason prefix the server seam synthesizes from a structured
/// `ActivityErrorKind::Retryable` wire failure (and the in-VM SDK encodes for
/// a typed `error.Retryable`).
pub(super) const RETRYABLE_REASON_PREFIX: &str = "retryable:";

/// Reason prefix of the ephemeral parked sentinel (#207). Never recorded to
/// history, never delivered to workflow code, never crossing the SDK wire —
/// it exists only to resolve a local pending waiter whose dispatch the server
/// parked for restart recovery during a graceful drain.
const PARKED_REASON_PREFIX: &str = "parked:";

/// The parked-dispatch sentinel the server's graceful drain resolves an
/// in-flight activity waiter with (#207).
///
/// Parking converges graceful shutdown onto the kill-9 recovery semantics: the
/// durable log keeps its dangling `ActivityScheduled`/`ActivityStarted` trail
/// (the proven re-dispatchable state the replay cursor exhausts into a live
/// re-dispatch), and this sentinel only unblocks the local completion wait so
/// process exit is never wedged on a blocking dispatcher thread. Defined here,
/// next to the reason-classification the retry loop consumes, and re-used by
/// `aion-server`'s drain path so both sides agree byte-for-byte.
pub const PARKED_ACTIVITY_REASON: &str = "parked:server-draining";

/// Whether a dispatcher failure reason is the parked-dispatch sentinel class
/// (#207): the `parked:` prefix, mirroring [`is_retryable_reason`]'s prefix
/// classification. Checked BEFORE the retry-policy filter — a parked dispatch
/// must never consume retry budget nor be delivered as a failure.
#[must_use]
pub fn is_parked_reason(reason: &str) -> bool {
    reason.starts_with(PARKED_REASON_PREFIX)
}

/// Whether a dispatcher failure reason is retryable-class.
///
/// True exactly when the reason carries the `retryable:` prefix — the string
/// form of the wire's structured retryability classification (see module
/// docs). Every other prefix (`terminal:`, `timeout:`, `cancelled:`, ...) and
/// every unprefixed engine failure is non-retryable and behaves as before.
pub(super) fn is_retryable_reason(reason: &str) -> bool {
    reason.starts_with(RETRYABLE_REASON_PREFIX)
}

/// The SDK-declared retry policy for one activity, decoded from the dispatch
/// `config` JSON.
///
/// Mirrors `aion/activity.RetryPolicy` in the Gleam SDK: `max_attempts` is the
/// TOTAL attempt budget (a policy of 3 means at most 2 retries after the first
/// delivery), and `backoff` is the delay strategy between attempts.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RetryPolicy {
    /// Total one-based attempt budget, including the first delivery.
    pub(super) max_attempts: u32,
    /// Delay strategy applied between a failed attempt and its retry.
    pub(super) backoff: Backoff,
}

/// Backoff strategy between attempts, mirroring `aion/activity.Backoff`.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Backoff {
    /// `initial * multiplier^(failed_attempt - 1)`, capped at `max`.
    Exponential {
        /// Delay after the first failed attempt.
        initial: Duration,
        /// Per-attempt growth factor.
        multiplier: f64,
        /// Upper bound on any single delay.
        max: Duration,
    },
    /// `initial + increment * (failed_attempt - 1)`, capped at `max`.
    Linear {
        /// Delay after the first failed attempt.
        initial: Duration,
        /// Additive per-attempt growth.
        increment: Duration,
        /// Upper bound on any single delay.
        max: Duration,
    },
    /// The same `delay` between every pair of attempts.
    Fixed {
        /// Constant inter-attempt delay.
        delay: Duration,
    },
}

impl Backoff {
    /// Delay before the retry that follows one-based `failed_attempt`.
    pub(super) fn delay_after(&self, failed_attempt: u32) -> Duration {
        let step = failed_attempt.saturating_sub(1);
        match self {
            Self::Fixed { delay } => *delay,
            Self::Linear {
                initial,
                increment,
                max,
            } => initial
                .saturating_add(increment.saturating_mul(step))
                .min(*max),
            Self::Exponential {
                initial,
                multiplier,
                max,
            } => {
                // f64 milliseconds keeps the growth math simple and saturating;
                // the cap bounds any precision loss to "capped at max".
                let factor = multiplier.powi(i32::try_from(step).unwrap_or(i32::MAX));
                let initial_ms = u64::try_from(initial.as_millis()).unwrap_or(u64::MAX);
                let scaled = precision_safe_mul(initial_ms, factor);
                Duration::from_millis(scaled).min(*max)
            }
        }
    }
}

/// `base * factor` in u64 milliseconds, saturating on overflow, NaN, or a
/// negative factor.
///
/// Precision-lossy by design (f64 mantissa < 64 bits): a backoff delay only
/// needs millisecond fidelity and the product is clamped to u64 range, so the
/// worst case of every lossy cast here is "capped at the policy max".
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn precision_safe_mul(base: u64, factor: f64) -> u64 {
    if !factor.is_finite() || factor <= 0.0 {
        return if factor <= 0.0 { base } else { u64::MAX };
    }
    let product = (base as f64) * factor;
    if product >= u64::MAX as f64 {
        u64::MAX
    } else {
        product as u64
    }
}

/// Decode the SDK-declared retry policy from the dispatch `config` JSON.
///
/// Returns `None` — run exactly once, today's behaviour — when the config
/// carries `"retry": null`, omits the field, or is malformed. A malformed
/// policy object is logged (never silently coerced) and treated as absent: a
/// broken declaration must not invent retry semantics the workflow author
/// never wrote.
pub(super) fn retry_policy_from_config(config: &str) -> Option<RetryPolicy> {
    let value: serde_json::Value = serde_json::from_str(config).ok()?;
    let retry = value.get("retry")?;
    if retry.is_null() {
        return None;
    }
    let policy = decode_policy(retry);
    if policy.is_none() {
        tracing::warn!(
            retry = %retry,
            "malformed SDK retry policy in dispatch config; treating the activity as \
             single-attempt (no retries)"
        );
    }
    policy
}

fn decode_policy(retry: &serde_json::Value) -> Option<RetryPolicy> {
    let max_attempts = u32::try_from(retry.get("max_attempts")?.as_u64()?).ok()?;
    if max_attempts == 0 {
        return None;
    }
    let backoff = retry.get("backoff")?;
    let kind = backoff.get("kind")?.as_str()?;
    let backoff = match kind {
        "exponential" => Backoff::Exponential {
            initial: millis_field(backoff, "initial_ms")?,
            multiplier: backoff.get("multiplier")?.as_f64()?,
            max: millis_field(backoff, "max_ms")?,
        },
        "linear" => Backoff::Linear {
            initial: millis_field(backoff, "initial_ms")?,
            increment: millis_field(backoff, "increment_ms")?,
            max: millis_field(backoff, "max_ms")?,
        },
        "fixed" => Backoff::Fixed {
            delay: millis_field(backoff, "delay_ms")?,
        },
        _ => return None,
    };
    Some(RetryPolicy {
        max_attempts,
        backoff,
    })
}

fn millis_field(value: &serde_json::Value, field: &str) -> Option<Duration> {
    Some(Duration::from_millis(value.get(field)?.as_u64()?))
}

/// Whether the activity (or its whole workflow) already reached a terminal
/// recorded outcome, so an in-flight retry loop must stop recording.
///
/// Guards the completion task's durable retry records against the settle
/// races the workflow thread can win while a retry sleeps or dispatches: a
/// `with_timeout` scope expiry records a terminal `ActivityFailed` for the
/// awaited ordinal, and a workflow terminal ends the run outright. Recording
/// a retry attempt AFTER such a terminal would make the replay walk read the
/// terminal as superseded — so the loop re-checks this under the recorder
/// lock before every append and aborts when the decision was already made.
///
/// Scans NEWEST-first because reopen supersedes terminals: a
/// [`Event::WorkflowReopened`] naming this activity re-drives it live, so the
/// most recent decisive event — the activity's own terminal, a workflow
/// terminal, or the reopen that supersedes them — wins.
pub(super) fn activity_settled(history: &[Event], activity_id: &ActivityId) -> bool {
    for event in history.iter().rev() {
        match event {
            Event::ActivityCompleted {
                activity_id: id, ..
            }
            | Event::ActivityCancelled {
                activity_id: id, ..
            } if id == activity_id => return true,
            Event::ActivityFailed {
                activity_id: id,
                error,
                ..
            } if id == activity_id && !error.is_retryable() => return true,
            Event::WorkflowReopened { reopened, .. } if reopened.contains(activity_id) => {
                // The reopen supersedes every prior terminal for this
                // activity AND the workflow terminal it belonged to; the
                // activity is live again.
                return false;
            }
            Event::WorkflowCompleted { .. }
            | Event::WorkflowFailed { .. }
            | Event::WorkflowCancelled { .. }
            | Event::WorkflowTimedOut { .. }
            | Event::WorkflowContinuedAsNew { .. } => return true,
            _ => {}
        }
    }
    false
}

/// The one-based attempt the NEXT live delivery of `activity_id` must carry,
/// derived from recorded history.
///
/// A fresh ordinal (no recorded attempts) is [`FIRST_DELIVERY_ATTEMPT`]. A
/// crash-recovery re-dispatch after a dangling retryable failure — recorded
/// attempt trail with no terminal — continues the trail instead of reusing an
/// attempt number, keeping `(workflow, activity, attempt)` a stable identity
/// across the restart. Legacy histories whose events decode the `attempt`
/// sentinel (`0`) resolve to [`FIRST_DELIVERY_ATTEMPT`] deterministically.
pub(super) fn next_delivery_attempt(history: &[Event], activity_id: &ActivityId) -> u32 {
    latest_recorded_attempt(history, activity_id).map_or(FIRST_DELIVERY_ATTEMPT, |attempt| {
        attempt.saturating_add(1).max(FIRST_DELIVERY_ATTEMPT)
    })
}

/// The highest attempt recorded for `activity_id` on any lifecycle event, or
/// `None` when the ordinal has no recorded attempt trail.
pub(super) fn latest_recorded_attempt(history: &[Event], activity_id: &ActivityId) -> Option<u32> {
    history
        .iter()
        .filter_map(|event| match event {
            Event::ActivityStarted {
                activity_id: id,
                attempt,
                ..
            }
            | Event::ActivityFailed {
                activity_id: id,
                attempt,
                ..
            }
            | Event::ActivityCompleted {
                activity_id: id,
                attempt,
                ..
            } if id == activity_id => Some(*attempt),
            _ => None,
        })
        .max()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aion_core::{
        ActivityError, ActivityErrorKind, ActivityId, ContentType, Event, EventEnvelope, Payload,
        WorkflowId,
    };

    use super::{
        Backoff, PARKED_ACTIVITY_REASON, RetryPolicy, activity_settled, is_parked_reason,
        is_retryable_reason, latest_recorded_attempt, next_delivery_attempt,
        retry_policy_from_config,
    };

    fn config_with(retry: &str) -> String {
        format!(r#"{{"retry":{retry},"timeout_ms":null,"labels":{{}}}}"#)
    }

    #[test]
    fn absent_null_and_malformed_policies_decode_to_no_retries() {
        assert_eq!(retry_policy_from_config("{}"), None);
        assert_eq!(retry_policy_from_config(&config_with("null")), None);
        assert_eq!(retry_policy_from_config("not json"), None);
        // Malformed object: missing backoff.
        assert_eq!(
            retry_policy_from_config(&config_with(r#"{"max_attempts":3}"#)),
            None
        );
        // Unknown backoff kind.
        assert_eq!(
            retry_policy_from_config(&config_with(
                r#"{"max_attempts":3,"backoff":{"kind":"warp","delay_ms":5}}"#
            )),
            None
        );
        // A zero attempt budget cannot mean "retry forever"; it decodes to
        // absent.
        assert_eq!(
            retry_policy_from_config(&config_with(
                r#"{"max_attempts":0,"backoff":{"kind":"fixed","delay_ms":5}}"#
            )),
            None
        );
    }

    #[test]
    fn sdk_shaped_policies_decode_exactly() {
        assert_eq!(
            retry_policy_from_config(&config_with(
                r#"{"max_attempts":3,"backoff":{"kind":"fixed","delay_ms":50}}"#
            )),
            Some(RetryPolicy {
                max_attempts: 3,
                backoff: Backoff::Fixed {
                    delay: Duration::from_millis(50)
                },
            })
        );
        assert_eq!(
            retry_policy_from_config(&config_with(
                r#"{"max_attempts":5,"backoff":{"kind":"exponential","initial_ms":100,"multiplier":2.0,"max_ms":1000}}"#
            )),
            Some(RetryPolicy {
                max_attempts: 5,
                backoff: Backoff::Exponential {
                    initial: Duration::from_millis(100),
                    multiplier: 2.0,
                    max: Duration::from_secs(1),
                },
            })
        );
        assert_eq!(
            retry_policy_from_config(&config_with(
                r#"{"max_attempts":4,"backoff":{"kind":"linear","initial_ms":10,"increment_ms":20,"max_ms":45}}"#
            )),
            Some(RetryPolicy {
                max_attempts: 4,
                backoff: Backoff::Linear {
                    initial: Duration::from_millis(10),
                    increment: Duration::from_millis(20),
                    max: Duration::from_millis(45),
                },
            })
        );
    }

    #[test]
    fn backoff_delays_grow_and_cap() {
        let exponential = Backoff::Exponential {
            initial: Duration::from_millis(100),
            multiplier: 2.0,
            max: Duration::from_millis(350),
        };
        assert_eq!(exponential.delay_after(1), Duration::from_millis(100));
        assert_eq!(exponential.delay_after(2), Duration::from_millis(200));
        assert_eq!(exponential.delay_after(3), Duration::from_millis(350));

        let linear = Backoff::Linear {
            initial: Duration::from_millis(10),
            increment: Duration::from_millis(20),
            max: Duration::from_millis(45),
        };
        assert_eq!(linear.delay_after(1), Duration::from_millis(10));
        assert_eq!(linear.delay_after(2), Duration::from_millis(30));
        assert_eq!(linear.delay_after(3), Duration::from_millis(45));

        let fixed = Backoff::Fixed {
            delay: Duration::from_millis(7),
        };
        assert_eq!(fixed.delay_after(1), Duration::from_millis(7));
        assert_eq!(fixed.delay_after(9), Duration::from_millis(7));
    }

    #[test]
    fn reason_classification_follows_the_wire_prefix_only() {
        assert!(is_retryable_reason("retryable:boom"));
        assert!(!is_retryable_reason("terminal:boom"));
        assert!(!is_retryable_reason("timeout:deadline expired"));
        assert!(!is_retryable_reason("cancelled:operator"));
        assert!(!is_retryable_reason("unprefixed engine failure"));
    }

    /// The parked sentinel (#207) is its own class: never retryable-class (it
    /// must not consume retry budget) and detected purely by the `parked:`
    /// prefix, exactly like the retryable classification.
    #[test]
    fn parked_classification_is_prefix_scoped_and_disjoint_from_retryable() {
        assert!(is_parked_reason(PARKED_ACTIVITY_REASON));
        assert!(is_parked_reason("parked:other-drain-vocabulary"));
        assert!(!is_parked_reason("retryable:worker lost"));
        assert!(!is_parked_reason("terminal:boom"));
        assert!(!is_parked_reason("unprefixed engine failure"));
        assert!(!is_retryable_reason(PARKED_ACTIVITY_REASON));
    }

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: WorkflowId::new_v4(),
        }
    }

    fn started(seq: u64, ordinal: u64, attempt: u32) -> Event {
        Event::ActivityStarted {
            envelope: envelope(seq),
            activity_id: ActivityId::from_sequence_position(ordinal),
            attempt,
        }
    }

    fn failed(seq: u64, ordinal: u64, attempt: u32, kind: ActivityErrorKind) -> Event {
        Event::ActivityFailed {
            envelope: envelope(seq),
            activity_id: ActivityId::from_sequence_position(ordinal),
            error: ActivityError {
                kind,
                message: "boom".to_owned(),
                details: None,
            },
            attempt,
        }
    }

    fn completed(seq: u64, ordinal: u64, attempt: u32) -> Event {
        Event::ActivityCompleted {
            envelope: envelope(seq),
            activity_id: ActivityId::from_sequence_position(ordinal),
            result: Payload::new(ContentType::Json, br#""r""#.to_vec()),
            attempt,
        }
    }

    #[test]
    fn settlement_tracks_terminal_outcomes_not_retryable_attempts() {
        let target = ActivityId::from_sequence_position(0);
        // A retryable attempt trail is NOT settled.
        assert!(!activity_settled(
            &[
                started(1, 0, 1),
                failed(2, 0, 1, ActivityErrorKind::Retryable)
            ],
            &target
        ));
        // A terminal failure settles it.
        assert!(activity_settled(
            &[failed(2, 0, 1, ActivityErrorKind::Terminal)],
            &target
        ));
        // A completion settles it.
        assert!(activity_settled(&[completed(3, 0, 2)], &target));
        // Another ordinal's terminal does not.
        assert!(!activity_settled(
            &[failed(2, 7, 1, ActivityErrorKind::Terminal)],
            &target
        ));
        // A workflow terminal settles every ordinal.
        assert!(activity_settled(
            &[Event::WorkflowFailed {
                envelope: envelope(4),
                error: aion_core::WorkflowError {
                    message: "done".to_owned(),
                    details: None,
                },
            }],
            &target
        ));
        // A reopen naming this activity supersedes both its terminal and the
        // workflow terminal: the activity is live again, NOT settled.
        assert!(!activity_settled(
            &[
                failed(2, 0, 3, ActivityErrorKind::Terminal),
                Event::WorkflowFailed {
                    envelope: envelope(3),
                    error: aion_core::WorkflowError {
                        message: "exhausted".to_owned(),
                        details: None,
                    },
                },
                Event::WorkflowReopened {
                    envelope: envelope(4),
                    run_id: aion_core::RunId::new_v4(),
                    reopened: vec![target.clone()],
                },
            ],
            &target
        ));
        // A reopen naming only OTHER activities leaves this one's terminal
        // decisive.
        assert!(activity_settled(
            &[
                failed(2, 0, 3, ActivityErrorKind::Terminal),
                Event::WorkflowReopened {
                    envelope: envelope(4),
                    run_id: aion_core::RunId::new_v4(),
                    reopened: vec![ActivityId::from_sequence_position(9)],
                },
            ],
            &target
        ));
    }

    #[test]
    fn next_attempt_continues_the_recorded_trail() {
        let target = ActivityId::from_sequence_position(0);
        assert_eq!(next_delivery_attempt(&[], &target), 1);
        assert_eq!(
            next_delivery_attempt(
                &[
                    started(1, 0, 1),
                    failed(2, 0, 1, ActivityErrorKind::Retryable),
                    started(3, 0, 2),
                    failed(4, 0, 2, ActivityErrorKind::Retryable),
                ],
                &target
            ),
            3
        );
        // The legacy attempt sentinel (0) resolves to the first delivery.
        assert_eq!(next_delivery_attempt(&[started(1, 0, 0)], &target), 1);
        // Another ordinal's trail is invisible.
        assert_eq!(next_delivery_attempt(&[started(1, 9, 4)], &target), 1);
        assert_eq!(
            latest_recorded_attempt(&[started(1, 0, 2), completed(2, 0, 2)], &target),
            Some(2)
        );
    }
}
