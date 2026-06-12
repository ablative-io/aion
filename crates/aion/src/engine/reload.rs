//! `Engine` runtime package-load seam: live load, routing, listing, unload.
//!
//! Decision record (#62, adopted 2026-06-12): D1 always-latest-at-record-time
//! with durable pinning, D2 manual unload with engine-enforced safety checks,
//! D3 embedded-only API with serde-ready types (the server endpoint is a
//! follow-up brief), D4 required `package_version` on start events.

use aion_core::Event;
use aion_package::ContentHash;

use crate::error::PinHolder;
use crate::loader::{LoadOutcome, WorkflowVersionInfo};
use crate::{EngineError, WorkflowCatalog};

use super::api::Engine;
use super::builder::{WorkflowPackageSource, package_from_source};

impl Engine {
    /// Loads a validated package into the running engine and atomically
    /// routes its workflow type's new dispatches to it.
    ///
    /// Every start that resolved before the route flip completes on the
    /// version it resolved (loads never unregister anything); every start
    /// after this call returns resolves the new version. Re-loading an
    /// already-loaded hash is idempotent (nothing registers,
    /// `freshly_loaded = false`) but still re-points the route at it —
    /// re-deploying a previously rolled-back version must take effect
    /// (`route_changed` reports whether it did).
    ///
    /// A successful load is persisted to the store (archive bytes plus,
    /// atomically, the route pointer) so the deploy survives a restart:
    /// startup reloads every persisted package before recovery resolves any
    /// run's recorded pinned version. Idempotent re-loads re-persist —
    /// re-deploying is a routing intent and the durable pointer must mirror
    /// it.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] once shutdown begins,
    /// [`EngineError::Load`] for archive, collision, registration, or
    /// entry-verification failures, and [`EngineError::ManifestMismatch`]
    /// when an idempotent re-load presents the resident content hash with a
    /// different manifest. On those failures the catalog is untouched:
    /// routing, loaded versions, and in-flight dispatches are unaffected.
    /// Returns [`EngineError::Store`] when the load committed but
    /// persistence failed — the version is live in THIS process yet would
    /// not survive a restart, so the deploy must not be reported applied;
    /// re-sending the same archive is idempotent and retries persistence.
    pub async fn load_package(
        &self,
        source: impl Into<WorkflowPackageSource>,
    ) -> Result<LoadOutcome, EngineError> {
        // A load is new-work admission, not a wind-down operation: refuse
        // after shutdown begins so modules never register into a dying VM.
        let operation = self.shutdown_gate.begin_start()?;
        let result = async {
            let package = package_from_source(source.into())?;
            let outcome = self
                .workflow_catalog()
                .load_package(self.runtime(), &package)
                .await?;
            crate::loader::persistence::persist_deployed_package(self.store().as_ref(), &package)
                .await?;
            Ok(outcome)
        }
        .await;
        drop(operation);
        result
    }

    /// Lists every loaded workflow version with its routing flag, sorted by
    /// `(workflow_type, loaded_at)`.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when the catalog lock is poisoned.
    pub fn list_workflow_versions(&self) -> Result<Vec<WorkflowVersionInfo>, EngineError> {
        self.workflow_catalog().versions()
    }

    /// Re-points routing for `workflow_type` at an already-loaded version
    /// (rollback / roll-forward). Atomic and idempotent.
    ///
    /// The pointer is persisted so the re-point survives a restart; startup
    /// restores persisted pointers after reloading persisted packages.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] once shutdown begins,
    /// [`EngineError::UnknownVersion`] naming the loaded set when
    /// `(type, version)` is not loaded — routing to a never-loaded hash is
    /// impossible — and [`EngineError::Store`] when the in-memory re-point
    /// committed but the durable pointer could not be written (retrying the
    /// same re-point is idempotent and re-attempts persistence).
    pub async fn route_workflow_version(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = async {
            self.workflow_catalog()
                .route_version(workflow_type, version)
                .await?;
            self.store()
                .put_package_route(workflow_type, &version.to_string())
                .await?;
            Ok(())
        }
        .await;
        drop(operation);
        result
    }

    /// Unloads a workflow version after verifying nothing pins it (D2).
    ///
    /// Refusal conditions, each typed and naming what pins the version:
    /// route-inactive is required (the route-active version of a type can
    /// never be unloaded), no in-flight start may pin it, no live registry
    /// handle may run on it, and no recoverable instance in the store —
    /// including a recorded-but-never-started child — may be pinned to it.
    ///
    /// The engine owns the mechanism; the embedding platform owns *when* to
    /// unload. There is no automatic garbage collection.
    ///
    /// Unload deletes the persisted deploy artifact too (a no-op for
    /// versions loaded from operator files, which were never persisted), so
    /// an unloaded version does not resurrect at the next restart.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] once shutdown begins,
    /// [`EngineError::UnknownVersion`] when `(type, version)` is not loaded,
    /// [`EngineError::RouteActive`] when the version is route-active,
    /// [`EngineError::VersionPinned`] naming the concrete pin holder (with
    /// the catalog restored untouched), [`EngineError::Store`] when the
    /// persisted artifact could not be deleted (the catalog is restored and
    /// the unload did not happen), and [`EngineError::Runtime`] when module
    /// unregistration fails after the catalog commit.
    pub async fn unload_workflow_version(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = self
            .unload_workflow_version_inner(workflow_type, version)
            .await;
        drop(operation);
        result
    }

    async fn unload_workflow_version_inner(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<(), EngineError> {
        let catalog = self.workflow_catalog();
        let _mutation = catalog.begin_mutation().await;
        // Swap the version out FIRST: from this instant no new resolution can
        // produce it, so the checks below cannot be invalidated by a racing
        // start (a start that already resolved holds a pin and is detected).
        let removed = catalog.swap_out_version(workflow_type, version)?;
        if let Err(error) = self
            .verify_unload_unpinned(catalog, workflow_type, version)
            .await
        {
            catalog.restore_version(removed)?;
            return Err(error);
        }
        // Delete the persisted artifact BEFORE unregistering modules: if the
        // delete fails the unload is rolled back wholesale, never leaving a
        // version that is gone from this process yet resurrects at the next
        // restart. Idempotent for never-persisted (operator-file) versions.
        if let Err(error) = self
            .store()
            .delete_package(workflow_type, &version.to_string())
            .await
        {
            catalog.restore_version(removed)?;
            return Err(error.into());
        }
        self.unregister_unloaded_modules(workflow_type, version, &removed)
    }

    /// Verifies no in-flight start, resident handle, or recoverable instance
    /// pins `(type, version)`.
    async fn verify_unload_unpinned(
        &self,
        catalog: &WorkflowCatalog,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<(), EngineError> {
        if catalog.has_pinned_starts(workflow_type, version)? {
            return Err(EngineError::VersionPinned {
                workflow_type: workflow_type.to_owned(),
                version: version.clone(),
                pinned_by: PinHolder::InFlightStart,
            });
        }

        for handle in self.registry().list()? {
            if handle.workflow_type() == workflow_type
                && handle.loaded_version() == version
                && !handle.cached_status().is_terminal()
            {
                return Err(EngineError::VersionPinned {
                    workflow_type: workflow_type.to_owned(),
                    version: version.clone(),
                    pinned_by: PinHolder::LiveRun {
                        workflow_id: handle.workflow_id().clone(),
                        run_id: handle.run_id().clone(),
                    },
                });
            }
        }

        let recorded = crate::loader::package_version_of(version);
        let store = self.store();
        for workflow_id in store.list_active().await? {
            let history = store.read_history(&workflow_id).await?;
            let current_run_pin = history.iter().rev().find_map(|event| match event {
                Event::WorkflowStarted {
                    workflow_type: started_type,
                    package_version,
                    ..
                } => Some(started_type == workflow_type && package_version == &recorded),
                _ => None,
            });
            if current_run_pin == Some(true) {
                return Err(EngineError::VersionPinned {
                    workflow_type: workflow_type.to_owned(),
                    version: version.clone(),
                    pinned_by: PinHolder::RecoverableRun {
                        workflow_id: workflow_id.clone(),
                    },
                });
            }
            if let Some(child_workflow_id) = self
                .recorded_unstarted_child_pin(&history, workflow_type, &recorded)
                .await?
            {
                return Err(EngineError::VersionPinned {
                    workflow_type: workflow_type.to_owned(),
                    version: version.clone(),
                    pinned_by: PinHolder::RecordedChild {
                        child_workflow_id,
                        recorded_by: workflow_id.clone(),
                    },
                });
            }
        }
        Ok(())
    }

    /// A recorded-but-never-started child pinned to the target version, if
    /// any: its `ChildWorkflowStarted` carries the version and its own
    /// history is still empty, so the crash-repair sweep would have to start
    /// it on exactly this version.
    async fn recorded_unstarted_child_pin(
        &self,
        parent_history: &[Event],
        workflow_type: &str,
        recorded: &aion_core::PackageVersion,
    ) -> Result<Option<aion_core::WorkflowId>, EngineError> {
        let store = self.store();
        for event in parent_history {
            let Event::ChildWorkflowStarted {
                child_workflow_id,
                workflow_type: child_type,
                package_version,
                ..
            } = event
            else {
                continue;
            };
            if child_type != workflow_type || package_version != recorded {
                continue;
            }
            if store.read_history(child_workflow_id).await?.is_empty() {
                return Ok(Some(child_workflow_id.clone()));
            }
        }
        Ok(None)
    }

    /// Unregisters the removed version's modules from the runtime, skipping
    /// host NIF modules that were never BEAM-registered.
    fn unregister_unloaded_modules(
        &self,
        workflow_type: &str,
        version: &ContentHash,
        removed: &crate::loader::catalog::RemovedVersion,
    ) -> Result<(), EngineError> {
        let nif_modules = self.runtime().registered_nif_modules();
        let mut failures = Vec::new();
        for deployed_name in removed.module_names() {
            let original = deployed_name.split('$').next().unwrap_or(deployed_name);
            if nif_modules.iter().any(|name| name == original) {
                continue;
            }
            if let Err(error) = self.runtime().unregister_module(deployed_name) {
                failures.push(format!("{deployed_name}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            // The catalog commit stands: the version is unloaded and its
            // names are unreachable (content-hash unique), but the runtime
            // retains orphaned module entries.
            Err(EngineError::Runtime {
                reason: format!(
                    "workflow `{workflow_type}` version `{version}` was removed from the catalog but module unregistration failed for {}",
                    failures.join(", ")
                ),
            })
        }
    }
}
