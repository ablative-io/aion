//! Runtime module registration helpers.

use beamr::loader::prepare_module;

use crate::{EngineError, RuntimeHandle};

impl RuntimeHandle {
    /// Register transformed BEAM bytes under their already-deployed module name.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when beamr cannot prepare the module bytes
    /// or when the deployed name still has retained old code in the registry.
    pub fn register_module(
        &self,
        deployed_name: &str,
        beam_bytes: &[u8],
    ) -> Result<(), EngineError> {
        let deployed_atom = self.atom_table.intern(deployed_name);
        if self.module_registry.lookup_old(deployed_atom).is_some() {
            return Err(runtime_error(format!(
                "cannot register deployed module `{deployed_name}` while old code is still retained"
            )));
        }

        let (mut module, _unresolved) = prepare_module(
            beam_bytes,
            &self.atom_table,
            &self.module_registry,
            self.native_registry.as_ref(),
        )
        .map_err(runtime_error_from_display)?;
        module.name = deployed_atom;
        self.module_registry.insert(module);

        Ok(())
    }

    /// Return true when a module has been registered in the embedded module registry.
    #[must_use]
    pub fn has_registered_module(&self, deployed_name: &str) -> bool {
        let module = self.atom_table.intern(deployed_name);
        self.module_registry.lookup(module).is_some()
    }

    /// Remove a module registered during a failed staged package load.
    pub(crate) fn unregister_module(&self, deployed_name: &str) -> Result<(), EngineError> {
        let module = self.atom_table.intern(deployed_name);
        if self.module_registry.delete_module(module) {
            Ok(())
        } else {
            Err(runtime_error(format!(
                "module `{deployed_name}` was not registered"
            )))
        }
    }
}

fn runtime_error(reason: String) -> EngineError {
    EngineError::Runtime { reason }
}

fn runtime_error_from_display(reason: impl std::fmt::Display) -> EngineError {
    runtime_error(reason.to_string())
}
