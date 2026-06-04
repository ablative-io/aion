//! Engine error taxonomy.

use aion_package::PackageError;
use aion_store::StoreError;

use crate::durability::DurabilityError;

/// Errors returned by the embedded workflow engine.
#[derive(thiserror::Error, Debug)]
pub enum EngineError {
    /// The builder was asked to construct an engine without an event store.
    #[error("engine store is required")]
    MissingStore,

    /// A workflow package failed to load or validate for engine registration.
    #[error("workflow package load failed: {reason}")]
    Load {
        /// Human-readable load failure reason.
        reason: String,
    },

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

    /// No live, durable, or loaded workflow was found for the request.
    #[error("workflow `{workflow_type}` was not found")]
    WorkflowNotFound {
        /// Logical workflow type requested by the caller.
        workflow_type: String,
    },

    /// Native implemented function registration failed.
    #[error("NIF registration failed: {reason}")]
    NifRegistration {
        /// Human-readable native implemented function registration failure reason.
        reason: String,
    },
}
