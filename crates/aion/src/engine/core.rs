//! `Engine` start, cancel, result, list, and shutdown support.

use std::sync::Arc;

use aion_store::EventStore;

use crate::{LoadedWorkflows, Registry, RuntimeHandle, SupervisionTree};

/// Live embedded workflow engine assembled by [`crate::EngineBuilder`].
pub struct Engine {
    store: Arc<dyn EventStore>,
    runtime: RuntimeHandle,
    loaded_workflows: LoadedWorkflows,
    registry: Registry,
    supervision: SupervisionTree,
}

impl Engine {
    /// Construct an engine from already-assembled components.
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn EventStore>,
        runtime: RuntimeHandle,
        loaded_workflows: LoadedWorkflows,
        registry: Registry,
        supervision: SupervisionTree,
    ) -> Self {
        Self {
            store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
        }
    }

    /// Event store used by lifecycle and delegated AD/AT operations.
    #[must_use]
    pub fn store(&self) -> Arc<dyn EventStore> {
        Arc::clone(&self.store)
    }

    /// Runtime boundary assembled for this engine.
    #[must_use]
    pub const fn runtime(&self) -> &RuntimeHandle {
        &self.runtime
    }

    /// Loaded workflow package registry.
    #[must_use]
    pub const fn loaded_workflows(&self) -> &LoadedWorkflows {
        &self.loaded_workflows
    }

    /// Active execution registry.
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Supervision tree snapshot/model.
    #[must_use]
    pub const fn supervision(&self) -> &SupervisionTree {
        &self.supervision
    }
}
