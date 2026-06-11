//! Engine error taxonomy.

use crate::schedule::{ScheduleError, ScheduleEvaluatorError};
use aion_core::{RunId, ScheduleId, WorkflowId};
use aion_package::PackageError;
use aion_store::StoreError;

use crate::durability::DurabilityError;

/// Errors returned by the embedded workflow engine.
#[derive(thiserror::Error, Debug)]
pub enum EngineError {
    /// The builder was asked to construct an engine without an event store.
    #[error("engine store is required")]
    MissingStore,

    /// The builder was asked to construct an engine without a visibility store.
    #[error(
        "engine visibility store is required; call EngineBuilder::visibility_store() or EngineBuilder::in_memory_visibility()"
    )]
    MissingVisibilityStore,

    /// A workflow package failed to load or validate for engine registration.
    #[error("workflow package load failed: {reason}")]
    Load {
        /// Human-readable load failure reason.
        reason: String,
    },

    /// The builder was given both `event_streaming` and an explicit event-publisher seam.
    #[error(
        "conflicting event publisher configuration: EngineBuilder::event_streaming installs the broadcast publisher and cannot be combined with EngineBuilder::event_publisher"
    )]
    ConflictingEventPublisher,

    /// Live event streaming setup failed.
    #[error("event streaming setup failed: {0}")]
    EventStreaming(#[from] crate::publish::PublishError),

    /// The configured event store returned an error.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// The durability recorder or replay path returned an error.
    #[error("durability error: {0}")]
    Durability(#[from] DurabilityError),

    /// A `.aion` package operation returned an error.
    #[error("package error: {0}")]
    Package(#[from] PackageError),

    /// The embedded runtime returned an error.
    #[error("runtime error: {reason}")]
    Runtime {
        /// Human-readable runtime failure reason.
        reason: String,
    },

    /// The active workflow registry lock was poisoned.
    #[error("active workflow registry lock was poisoned")]
    RegistryPoisoned,

    /// The engine is already shutting down and no new workflow starts are accepted.
    #[error("engine is shutting down")]
    ShuttingDown,

    /// No live, durable, or loaded workflow was found for the request.
    #[error("workflow `{workflow_type}` was not found")]
    WorkflowNotFound {
        /// Logical workflow type requested by the caller.
        workflow_type: String,
    },

    /// No durable schedule was found for the request.
    #[error("schedule `{schedule_id}` was not found")]
    ScheduleNotFound {
        /// Schedule identifier requested by the caller.
        schedule_id: ScheduleId,
    },

    /// Schedule trigger, projection, or evaluator side effect failed.
    #[error("schedule error: {reason}")]
    Schedule {
        /// Human-readable schedule failure reason.
        reason: String,
    },

    /// Native implemented function registration failed.
    #[error("NIF registration failed: {reason}")]
    NifRegistration {
        /// Human-readable native implemented function registration failure reason.
        reason: String,
    },

    /// Signal routing failed after the target was resolved.
    #[error("signal router error: {0}")]
    SignalRouter(#[from] SignalRouterError),
}

/// Errors surfaced by the signal routing boundary.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum SignalRouterError {
    /// The target workflow is terminal and cannot receive new signals.
    #[error("workflow {workflow_id}/{run_id} is terminal")]
    Terminal {
        /// Target workflow id.
        workflow_id: WorkflowId,
        /// Target run id.
        run_id: RunId,
    },

    /// The router could not defer a recorded non-resident signal.
    #[error("signal resume handoff failed: {reason}")]
    Handoff {
        /// Human-readable handoff failure reason.
        reason: String,
    },

    /// The signal was durably recorded but could not be delivered to the live mailbox.
    #[error(
        "signal `{signal_name}` for workflow {workflow_id}/{run_id} could not be delivered to process {process_id}: {reason}"
    )]
    DeliveryFailed {
        /// Target workflow id.
        workflow_id: WorkflowId,
        /// Target run id.
        run_id: RunId,
        /// Embedded runtime process identifier selected for delivery.
        process_id: u64,
        /// Signal name that was recorded and attempted.
        signal_name: String,
        /// Human-readable delivery failure reason.
        reason: String,
    },
}

impl From<ScheduleError> for EngineError {
    fn from(error: ScheduleError) -> Self {
        Self::Schedule {
            reason: error.to_string(),
        }
    }
}

impl From<ScheduleEvaluatorError> for EngineError {
    fn from(error: ScheduleEvaluatorError) -> Self {
        match error {
            ScheduleEvaluatorError::ScheduleNotFound { schedule_id } => {
                Self::ScheduleNotFound { schedule_id }
            }
            other => Self::Schedule {
                reason: other.to_string(),
            },
        }
    }
}
