//! Error types shared by workflow and activity histories.

use serde::{Deserialize, Serialize};

use crate::Payload;

/// Classification for an activity failure.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ActivityErrorKind {
    /// The activity failure may be retried according to the activity's retry policy.
    Retryable,
    /// The activity failure is terminal and must not be retried.
    Terminal,
}

/// Failure reported by an activity execution.
///
/// The engine consults [`ActivityError::is_retryable`] to decide whether to
/// apply the activity's retry policy or fail the workflow.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ActivityError {
    /// Explicit retryability classification for this activity failure.
    pub kind: ActivityErrorKind,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured details carried as an opaque payload.
    pub details: Option<Payload>,
}

impl ActivityError {
    /// Returns whether the engine may retry this activity failure.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self.kind, ActivityErrorKind::Retryable)
    }
}

/// Terminal failure reported by a workflow execution.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorkflowError {
    /// Human-readable error message.
    pub message: String,
    /// Optional structured details carried as an opaque payload.
    pub details: Option<Payload>,
}

impl From<ActivityError> for WorkflowError {
    fn from(error: ActivityError) -> Self {
        Self {
            message: error.message,
            details: error.details,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ActivityError, ActivityErrorKind, WorkflowError};
    use crate::Payload;

    #[test]
    fn activity_error_reports_retryable_classification() {
        let error = ActivityError {
            kind: ActivityErrorKind::Retryable,
            message: String::from("temporary outage"),
            details: None,
        };

        assert!(error.is_retryable());
    }

    #[test]
    fn activity_error_reports_terminal_classification() {
        let error = ActivityError {
            kind: ActivityErrorKind::Terminal,
            message: String::from("invalid request"),
            details: None,
        };

        assert!(!error.is_retryable());
    }

    #[test]
    fn errors_round_trip_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let activity_error = ActivityError {
            kind: ActivityErrorKind::Retryable,
            message: String::from("connection reset"),
            details: Some(Payload::from_json(&json!({"retry_after_ms": 500}))?),
        };
        let json = serde_json::to_string(&activity_error)?;
        let decoded: ActivityError = serde_json::from_str(&json)?;
        assert_eq!(activity_error, decoded);

        let workflow_error = WorkflowError {
            message: String::from("workflow failed"),
            details: None,
        };
        let json = serde_json::to_string(&workflow_error)?;
        let decoded: WorkflowError = serde_json::from_str(&json)?;
        assert_eq!(workflow_error, decoded);

        Ok(())
    }

    #[test]
    fn workflow_error_from_activity_error_preserves_message_and_details()
    -> Result<(), Box<dyn std::error::Error>> {
        let details = Payload::from_json(&json!({"code": "rate_limited", "after_ms": 1000}))?;
        let activity_error = ActivityError {
            kind: ActivityErrorKind::Terminal,
            message: String::from("activity failed permanently"),
            details: Some(details.clone()),
        };

        let workflow_error = WorkflowError::from(activity_error);

        assert_eq!(workflow_error.message, "activity failed permanently");
        assert_eq!(workflow_error.details, Some(details));
        Ok(())
    }
}
