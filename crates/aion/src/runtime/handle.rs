//! `RuntimeHandle` spawn, register, cancel, and shutdown support.

use std::sync::Arc;

use beamr::atom::AtomTable;
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
}

impl RuntimeHandle {
    /// Construct and start an embedded runtime from builder-supplied config.
    pub fn new(config: RuntimeConfig) -> Result<Self, EngineError> {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let module_registry = Arc::new(ModuleRegistry::new());
        let scheduler_config = SchedulerConfig {
            thread_count: config.thread_count,
        };
        let scheduler = Scheduler::with_code_server(
            scheduler_config,
            Arc::clone(&module_registry),
            Arc::clone(&atom_table),
            Arc::new(BifRegistryImpl::new()),
        )
        .map_err(runtime_error)?;

        Ok(Self {
            scheduler,
            atom_table,
            module_registry,
        })
    }

    /// Spawn a top-level workflow process at a deployed module/function entrypoint.
    pub fn spawn_workflow(
        &self,
        deployed_module: &str,
        function: &str,
        input: RuntimeInput,
    ) -> Result<Pid, EngineError> {
        self.spawn_process(deployed_module, function, input)
    }

    /// Spawn an activity child process linked to its workflow parent.
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
            .map_err(runtime_error)
    }

    /// Register transformed BEAM bytes under their already-deployed module name.
    pub fn register_module(
        &self,
        deployed_name: &str,
        beam_bytes: &[u8],
    ) -> Result<(), EngineError> {
        let loaded = self
            .scheduler
            .hot_load_module(beam_bytes)
            .map_err(runtime_error)?;
        let loaded_name = self
            .atom_table
            .resolve(loaded.module_name)
            .ok_or_else(|| runtime_error("loaded module name is not interned".to_owned()))?;

        if loaded_name != deployed_name {
            return Err(runtime_error(format!(
                "loaded module `{loaded_name}` did not match deployed name `{deployed_name}`"
            )));
        }

        Ok(())
    }

    /// Cancel a live process by PID.
    pub fn cancel_pid(&self, pid: Pid) -> Result<(), EngineError> {
        self.ensure_live_pid(pid)?;
        self.scheduler.terminate_process(pid, ExitReason::Kill);
        Ok(())
    }

    /// Shut down the embedded scheduler and wait for worker threads to stop.
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
            .map_err(runtime_error)
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
}

fn runtime_error(reason: impl ToString) -> EngineError {
    EngineError::Runtime {
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeHandle, RuntimeInput};
    use crate::runtime::RuntimeConfig;

    fn assert_send_sync<T: Send + Sync>() {}

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("runtime")
            .join(name);
        std::fs::read(path).unwrap_or_else(|error| panic!("read fixture {name}: {error}"))
    }

    #[test]
    fn runtime_handle_is_send_sync() {
        assert_send_sync::<RuntimeHandle>();
    }

    #[test]
    fn registers_spawns_and_shuts_down() {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))
            .unwrap_or_else(|error| panic!("runtime starts: {error}"));
        runtime
            .register_module("counter", &fixture("counter_v1.beam"))
            .unwrap_or_else(|error| panic!("module registers: {error}"));

        let pid = runtime
            .spawn_workflow("counter", "version", RuntimeInput::default())
            .unwrap_or_else(|error| panic!("workflow spawns: {error}"));
        assert!(runtime.cancel_pid(pid).is_ok());
        runtime
            .shutdown()
            .unwrap_or_else(|error| panic!("runtime shuts down: {error}"));
    }
}
