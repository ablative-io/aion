//! `Command` and `Resolution` types at the AD seam.

use aion_core::{ActivityError, Payload, WithTimeoutOutcome, WorkflowError, WorkflowId};
use chrono::{DateTime, Utc};

use crate::durability::{CorrelationKey, SignalDelivery};

/// World-touching workflow intent presented to the durability resolver.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Run an activity identified by a deterministic activity scheduling key.
    RunActivity {
        /// Correlation key that must match the recorded activity schedule.
        key: CorrelationKey,
        /// Activity type selected by workflow code.
        activity_type: String,
        /// Opaque activity input payload.
        input: Payload,
    },
    /// Await the next matching signal delivery.
    AwaitSignal {
        /// Correlation key naming the signal and occurrence index to deliver.
        key: CorrelationKey,
    },
    /// Send a signal to another workflow.
    SendSignal {
        /// Correlation key naming the sent signal occurrence.
        key: CorrelationKey,
        /// Delivery selected by workflow code.
        delivery: SignalDelivery,
    },
    /// Start or await a timer identified by a deterministic timer key.
    StartTimer {
        /// Correlation key that must match the recorded timer start.
        key: CorrelationKey,
        /// UTC instant at which the timer becomes eligible to fire.
        fire_at: DateTime<Utc>,
    },
    /// Spawn a child workflow identified by a deterministic child scheduling key.
    SpawnChild {
        /// Correlation key that must match the recorded child-workflow start.
        key: CorrelationKey,
        /// Child workflow type selected by workflow code.
        workflow_type: String,
        /// Opaque child workflow input payload.
        input: Payload,
    },
    /// Await a previously spawned child workflow's terminal outcome.
    AwaitChild {
        /// Child workflow identity returned by [`Command::SpawnChild`].
        child_workflow_id: WorkflowId,
    },
    /// Complete the current workflow with a terminal result payload.
    ///
    /// Workflow completion is terminal intent rather than a cursor-matched replay family, so it has
    /// no correlation key in AD-004.
    CompleteWorkflow {
        /// Opaque workflow result payload.
        result: Payload,
    },
}

impl Command {
    /// Returns the correlation key for cursor-matched commands.
    ///
    /// [`Command::CompleteWorkflow`] returns `None` because completion is terminal intent, not a
    /// recorded event family consumed by [`crate::durability::HistoryCursor`].
    #[must_use]
    pub const fn key(&self) -> Option<&CorrelationKey> {
        match self {
            Self::RunActivity { key, .. }
            | Self::AwaitSignal { key }
            | Self::SendSignal { key, .. }
            | Self::StartTimer { key, .. }
            | Self::SpawnChild { key, .. } => Some(key),
            Self::AwaitChild { .. } | Self::CompleteWorkflow { .. } => None,
        }
    }
}

/// Recorded outcome produced by resolving a command against history.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// A recorded activity completion with its result payload.
    ActivityCompleted(Payload),
    /// A recorded terminal activity failure.
    ActivityFailedTerminal(ActivityError),
    /// A recorded timer firing.
    TimerFired,
    /// A recorded timer start without a terminal timer outcome yet.
    TimerStarted,
    /// A recorded signal delivery with its payload.
    SignalDelivered(Payload),
    /// A recorded timer cancellation.
    TimerCancelled,
    /// A recorded `with_timeout` terminal outcome with an optional result payload.
    WithTimeout {
        /// Recorded timeout outcome.
        outcome: WithTimeoutOutcome,
        /// JSON-encoded BEAM term payload for completed operation results.
        result: Option<Payload>,
    },
    /// A recorded successful signal send.
    SignalSent,
    /// A recorded child workflow start with its child identifier.
    ChildStarted(WorkflowId),
    /// A recorded child workflow completion with its result payload.
    ChildCompleted(Payload),
    /// A recorded child workflow failure.
    ChildFailed(WorkflowError),
}

/// Outcome of passing a [`Command`] through the durability resolver.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolveOutcome {
    /// The command was satisfied from recorded history without invoking live side effects.
    Recorded(Resolution),
    /// Recorded history is exhausted; ownership must hand off to live execution.
    ResumeLive,
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityErrorKind, TimerId};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::{Command, Resolution};
    use crate::durability::CorrelationKey;

    fn payload(label: &str) -> Result<aion_core::Payload, Box<dyn std::error::Error>> {
        Ok(aion_core::Payload::from_json(&json!({ "label": label }))?)
    }

    #[test]
    fn command_keys_round_trip_for_cursor_matched_variants()
    -> Result<(), Box<dyn std::error::Error>> {
        let activity_key = CorrelationKey::Activity(1);
        let signal_key = CorrelationKey::Signal {
            name: "ready".to_owned(),
            index: 0,
        };
        let timer_key = CorrelationKey::Timer(TimerId::anonymous(2));
        let child_key = CorrelationKey::Child(3);
        let fire_at = Utc
            .timestamp_opt(10, 0)
            .single()
            .ok_or_else(|| "invalid timestamp".to_owned())?;
        let commands = vec![
            (
                Command::RunActivity {
                    key: activity_key.clone(),
                    activity_type: "activity".to_owned(),
                    input: payload("activity-input")?,
                },
                Some(activity_key),
            ),
            (
                Command::AwaitSignal {
                    key: signal_key.clone(),
                },
                Some(signal_key),
            ),
            (
                Command::StartTimer {
                    key: timer_key.clone(),
                    fire_at,
                },
                Some(timer_key),
            ),
            (
                Command::SpawnChild {
                    key: child_key.clone(),
                    workflow_type: "child".to_owned(),
                    input: payload("child-input")?,
                },
                Some(child_key),
            ),
            (
                Command::AwaitChild {
                    child_workflow_id: aion_core::WorkflowId::new_v4(),
                },
                None,
            ),
            (
                Command::CompleteWorkflow {
                    result: payload("workflow-result")?,
                },
                None,
            ),
        ];

        for (command, expected_key) in commands {
            assert_eq!(command.key(), expected_key.as_ref());
        }
        Ok(())
    }

    #[test]
    fn resolutions_round_trip_by_equality() -> Result<(), Box<dyn std::error::Error>> {
        let activity_error = aion_core::ActivityError {
            kind: ActivityErrorKind::Terminal,
            message: "terminal".to_owned(),
            details: None,
        };
        let workflow_error = aion_core::WorkflowError {
            message: "child failed".to_owned(),
            details: None,
        };
        let resolutions = vec![
            Resolution::ActivityCompleted(payload("activity-result")?),
            Resolution::ActivityFailedTerminal(activity_error),
            Resolution::TimerFired,
            Resolution::SignalDelivered(payload("signal")?),
            Resolution::ChildStarted(aion_core::WorkflowId::new_v4()),
            Resolution::ChildCompleted(payload("child-result")?),
            Resolution::ChildFailed(workflow_error),
        ];

        for resolution in resolutions {
            let cloned = resolution.clone();
            assert_eq!(cloned, resolution);
        }
        Ok(())
    }
}
