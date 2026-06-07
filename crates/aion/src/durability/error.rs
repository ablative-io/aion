//! `NonDeterminismError` and `DurabilityError` taxonomy.

use aion_core::{SearchAttributeError, WorkflowId};
use aion_store::StoreError;

/// A deterministic replay mismatch between the workflow command stream and recorded history.
#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
#[error(
    "non-determinism in workflow {workflow_id} at sequence {seq}: expected {expected}, found {found}"
)]
pub struct NonDeterminismError {
    /// Workflow whose recorded history diverged from the replayed command stream.
    pub workflow_id: WorkflowId,
    /// Sequence position of the recorded event at the cursor mismatch.
    pub seq: u64,
    /// Shape of the command the workflow issued, including family and correlation key.
    pub expected: String,
    /// Shape of the recorded event found at the cursor position, including family and key.
    pub found: String,
}

/// Errors returned by durability recording, replay, and recovery operations.
#[derive(thiserror::Error, Debug)]
pub enum DurabilityError {
    /// The backing event store rejected or failed a durability operation.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// Replay detected that workflow code no longer matches recorded history.
    #[error("non-determinism violation: {0}")]
    NonDeterminism(#[from] NonDeterminismError),

    /// Recorded history is malformed or internally inconsistent.
    #[error("history shape error: {reason}")]
    HistoryShape {
        /// Human-readable description of the malformed recorded history.
        reason: String,
    },

    /// A search attribute update did not satisfy the registered schema.
    #[error("search attribute validation error: {0}")]
    SearchAttribute(#[from] SearchAttributeError),
}

#[cfg(test)]
mod tests {
    use super::{DurabilityError, NonDeterminismError};
    use aion_core::WorkflowId;
    use aion_store::StoreError;

    fn non_determinism_error() -> NonDeterminismError {
        NonDeterminismError {
            workflow_id: WorkflowId::new(uuid::Uuid::nil()),
            seq: 42,
            expected: "activity schedule ordinal 7".to_owned(),
            found: "timer fired timer:named:deadline".to_owned(),
        }
    }

    #[test]
    fn non_determinism_display_includes_context() {
        let error = non_determinism_error();

        let message = error.to_string();

        assert!(message.contains("00000000-0000-0000-0000-000000000000"));
        assert!(message.contains("42"));
        assert!(message.contains("activity schedule ordinal 7"));
        assert!(message.contains("timer fired timer:named:deadline"));
    }

    #[test]
    fn durability_error_display_mentions_underlying_cause() {
        let store = DurabilityError::Store(StoreError::SequenceConflict {
            expected: 10,
            found: 11,
        });
        let non_determinism = DurabilityError::NonDeterminism(non_determinism_error());
        let history_shape = DurabilityError::HistoryShape {
            reason: "activity result without preceding schedule".to_owned(),
        };

        let store_message = store.to_string();
        let non_determinism_message = non_determinism.to_string();
        let history_shape_message = history_shape.to_string();

        assert!(!store_message.is_empty());
        assert!(store_message.contains("sequence conflict"));
        assert!(!non_determinism_message.is_empty());
        assert!(non_determinism_message.contains("activity schedule ordinal 7"));
        assert!(non_determinism_message.contains("timer fired timer:named:deadline"));
        assert!(!history_shape_message.is_empty());
        assert!(history_shape_message.contains("activity result without preceding schedule"));
    }
}
