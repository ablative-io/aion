//! `RuntimeHandle` spawn, register, cancel, and shutdown support.

use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::loader::prepare_module;
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

use crate::error::EngineError;

use super::config::RuntimeConfig;

/// Local BEAM process identifier exposed by the runtime boundary.
pub type Pid = u64;

/// Runtime-owned workflow or activity input terms.
///
/// The wrapper keeps the beamr term representation inside the runtime module
/// while later lifecycle and payload code decide how durable payloads become VM
/// terms.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeInput {
    terms: Vec<Term>,
}

impl RuntimeInput {
    fn into_terms(self) -> Vec<Term> {
        self.terms
    }
}

/// Handle to the embedded beamr scheduler and code-server state.
pub struct RuntimeHandle {
    scheduler: Scheduler,
    atom_table: Arc<AtomTable>,
    module_registry: Arc<ModuleRegistry>,
    bif_registry: Arc<BifRegistryImpl>,
}

impl RuntimeHandle {
    /// Construct and start an embedded runtime from builder-supplied config.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when beamr cannot start its scheduler.
    pub fn new(config: RuntimeConfig) -> Result<Self, EngineError> {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let module_registry = Arc::new(ModuleRegistry::new());
        let bif_registry = Arc::new(BifRegistryImpl::new());
        let scheduler_config = SchedulerConfig {
            thread_count: config.thread_count,
        };
        let scheduler = Scheduler::with_code_server(
            scheduler_config,
            Arc::clone(&module_registry),
            Arc::clone(&atom_table),
            Arc::clone(&bif_registry),
        )
        .map_err(runtime_error_from_display)?;

        Ok(Self {
            scheduler,
            atom_table,
            module_registry,
            bif_registry,
        })
    }

    /// Spawn a top-level workflow process at a deployed module/function entrypoint.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the module/function/arity cannot be
    /// resolved or beamr rejects the spawn request.
    pub fn spawn_workflow(
        &self,
        deployed_module: &str,
        function: &str,
        input: RuntimeInput,
    ) -> Result<Pid, EngineError> {
        self.spawn_process(deployed_module, function, input)
    }

    /// Spawn an activity child process linked to its workflow parent.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the parent process is not live, the
    /// module/function/arity cannot be resolved, or beamr rejects the linked
    /// spawn request.
    pub fn spawn_activity(
        &self,
        parent_pid: Pid,
        deployed_module: &str,
        function: &str,
        input: RuntimeInput,
    ) -> Result<Pid, EngineError> {
        self.ensure_live_pid(parent_pid)?;
        let module = self.atom_table.intern(deployed_module);
        let function = self.atom_table.intern(function);
        self.scheduler
            .spawn_link(parent_pid, module, function, input.into_terms())
            .map_err(runtime_error_from_display)
    }

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
            self.bif_registry.as_ref(),
        )
        .map_err(runtime_error_from_display)?;
        module.name = deployed_atom;
        self.module_registry.insert(module);

        Ok(())
    }

    /// Cancel a live process by PID.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when `pid` is not live.
    pub fn cancel_pid(&self, pid: Pid) -> Result<(), EngineError> {
        self.ensure_live_pid(pid)?;
        self.scheduler.terminate_process(pid, ExitReason::Kill);
        Ok(())
    }

    /// Shut down the embedded scheduler and wait for worker threads to stop.
    ///
    /// # Errors
    ///
    /// Currently infallible; reserved for typed runtime shutdown failures.
    pub fn shutdown(&self) -> Result<(), EngineError> {
        self.scheduler.shutdown();
        Ok(())
    }

    fn spawn_process(
        &self,
        deployed_module: &str,
        function: &str,
        input: RuntimeInput,
    ) -> Result<Pid, EngineError> {
        let module = self.atom_table.intern(deployed_module);
        let function = self.atom_table.intern(function);
        self.scheduler
            .spawn(module, function, input.into_terms())
            .map_err(runtime_error_from_display)
    }

    fn ensure_live_pid(&self, pid: Pid) -> Result<(), EngineError> {
        if self.scheduler.process_table().get(pid).is_some() {
            Ok(())
        } else {
            Err(runtime_error(format!("process {pid} is not live")))
        }
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

#[cfg(test)]
mod tests {
    use super::{RuntimeHandle, RuntimeInput};
    use crate::runtime::RuntimeConfig;

    fn assert_send_sync<T: Send + Sync>() {}

    fn fixture(name: &str) -> std::io::Result<Vec<u8>> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("runtime")
            .join(name);
        std::fs::read(path)
    }

    #[test]
    fn runtime_handle_is_send_sync() {
        assert_send_sync::<RuntimeHandle>();
    }

    #[test]
    fn registers_spawns_and_shuts_down() -> Result<(), Box<dyn std::error::Error>> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
        runtime.register_module("counter", &fixture("counter_v1.beam")?)?;

        let pid = runtime.spawn_workflow("counter", "version", RuntimeInput::default())?;
        assert!(runtime.cancel_pid(pid).is_ok());
        runtime.shutdown()?;
        Ok(())
    }
}
