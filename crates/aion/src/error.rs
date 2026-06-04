//! Engine error taxonomy.

use aion_core::WorkflowId;
use aion_package::PackageError;
use aion_store::StoreError;

/// Errors returned by the embedded workflow engine.
#[derive(thiserror::Error, Debug)]
pub enum EngineError {
    /// The builder was asked to construct an engine without an event store.
    #[error("engine store is required")]
    MissingStore,

    /// A workflow package failed to load or validate for engine registration.
    #[error("workflow package load failed: {reason}")]
    Load { reason: String },

    /// The configured event store returned an error.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// A `.aion` package operation returned an error.
    #[error("package error: {0}")]
    Package(#[from] PackageError),

    /// The embedded runtime returned an error.
    #[error("runtime error: {reason}")]
    Runtime { reason: String },

    /// The active workflow registry lock was poisoned.
    #[error("active workflow registry lock was poisoned")]
    RegistryPoisoned,

    /// No live or durable workflow was found for the requested identifier.
    #[error("workflow {0} was not found")]
    WorkflowNotFound(WorkflowId),

    /// Native implemented function registration failed.
    #[error("NIF registration failed: {reason}")]
    NifRegistration { reason: String },
}
