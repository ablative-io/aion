//! Test-only catalog seeding helpers shared by in-crate unit suites.

use aion_package::{ContentHash, ManifestVersion};
use chrono::Utc;

use super::{CatalogEntry, WorkflowCatalog};
use crate::error::EngineError;
use crate::loader::load::{LoadedWorkflow, load_error};

impl WorkflowCatalog {
    /// Returns true when the catalog has committed the deployed module name.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn has_registered_module(&self, deployed_name: &str) -> bool {
        self.current()
            .map(|snapshot| snapshot.registered_modules.contains_key(deployed_name))
            .unwrap_or(false)
    }

    /// Records a loaded workflow entry without runtime registration for tests.
    #[cfg(test)]
    pub(crate) fn note_loaded_workflow_for_test(
        &self,
        workflow_type: impl Into<String>,
        deployed_entry_module: impl Into<String>,
        entry_function: impl Into<String>,
        version: ContentHash,
    ) -> LoadedWorkflow {
        let record = LoadedWorkflow::from_parts(
            workflow_type.into(),
            deployed_entry_module.into(),
            entry_function.into(),
            version,
        );
        let Ok(snapshot) = self.current() else {
            return record;
        };
        let mut next = (*snapshot).clone();
        next.by_version.insert(
            (record.workflow_type().to_owned(), record.version().clone()),
            CatalogEntry {
                workflow: record.clone(),
                manifest_version: ManifestVersion::new("test"),
                loaded_at: Utc::now(),
            },
        );
        next.routed
            .insert(record.workflow_type().to_owned(), record.version().clone());
        let _ = self.install(next);
        record
    }

    /// Forces a committed module-name mapping for collision tests.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Load`] when the name is already mapped to a
    /// different version.
    #[cfg(test)]
    pub(crate) fn note_registered_module(
        &self,
        deployed_name: impl Into<String>,
        version: ContentHash,
    ) -> Result<(), EngineError> {
        let deployed_name = deployed_name.into();
        let snapshot = self.current()?;
        match snapshot.registered_modules.get(&deployed_name) {
            Some(existing) if existing != &version => Err(load_error(format!(
                "deployed module `{deployed_name}` is already registered for content hash `{existing}`, not `{version}`"
            ))),
            Some(_) => Ok(()),
            None => {
                let mut next = (*snapshot).clone();
                next.registered_modules.insert(deployed_name, version);
                self.install(next)
            }
        }
    }
}
