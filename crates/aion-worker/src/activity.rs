//! `Activity` trait, `ActivityFailure`, and typed registration.

use aion_core::{ActivityError, ActivityErrorKind, Payload};

/// Explicit retryability classification for an activity failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classification {
    /// The engine may retry the activity according to policy.
    Retryable,
    /// The activity failure is permanent and must not be retried.
    Terminal,
}

/// Handler-returned failure with explicit retryability classification.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ActivityFailure {
    classification: Classification,
    message: String,
    detail: Option<Payload>,
}

impl ActivityFailure {
    /// Creates a retryable activity failure.
    #[must_use]
    pub fn retryable(message: impl Into<String>) -> Self {
        Self::new(Classification::Retryable, message, None)
    }

    /// Creates a terminal activity failure.
    #[must_use]
    pub fn terminal(message: impl Into<String>) -> Self {
        Self::new(Classification::Terminal, message, None)
    }

    /// Attaches opaque structured detail to this failure.
    #[must_use]
    pub fn with_detail(mut self, detail: Payload) -> Self {
        self.detail = Some(detail);
        self
    }

    /// Returns the explicit retryability classification.
    #[must_use]
    pub const fn classification(&self) -> &Classification {
        &self.classification
    }

    /// Returns the human-readable failure message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the optional structured failure detail.
    #[must_use]
    pub const fn detail(&self) -> Option<&Payload> {
        self.detail.as_ref()
    }

    fn new(
        classification: Classification,
        message: impl Into<String>,
        detail: Option<Payload>,
    ) -> Self {
        Self {
            classification,
            message: message.into(),
            detail,
        }
    }
}

impl From<Classification> for ActivityErrorKind {
    fn from(value: Classification) -> Self {
        match value {
            Classification::Retryable => Self::Retryable,
            Classification::Terminal => Self::Terminal,
        }
    }
}

impl From<ActivityFailure> for ActivityError {
    fn from(value: ActivityFailure) -> Self {
        Self {
            kind: ActivityErrorKind::from(value.classification),
            message: value.message,
            details: value.detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use aion_core::ActivityError;
    use aion_proto::{ProtoActivityError, ProtoActivityErrorKind};

    use super::ActivityFailure;

    #[test]
    fn retryable_and_terminal_failures_map_to_distinct_wire_classifications() {
        let retryable = ActivityFailure::retryable("temporary outage");
        let terminal = ActivityFailure::terminal("invalid request");

        let retryable_core = ActivityError::from(retryable);
        let terminal_core = ActivityError::from(terminal);
        let retryable_wire = ProtoActivityError::from(retryable_core);
        let terminal_wire = ProtoActivityError::from(terminal_core);

        assert_eq!(
            retryable_wire.kind,
            ProtoActivityErrorKind::Retryable as i32
        );
        assert_eq!(terminal_wire.kind, ProtoActivityErrorKind::Terminal as i32);
    }
}
