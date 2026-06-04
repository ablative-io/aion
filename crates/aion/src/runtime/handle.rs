//! `RuntimeHandle` spawn, register, cancel, and shutdown support.

use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::module::ModuleRegistry;
use beamr::native::{BifRegistryImpl, NativeRegistrationError};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

use crate::error::EngineError;

use super::config::RuntimeConfig;
use super::nif::{Mfa, NifRegistration};

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
    native_registry: Arc<BifRegistryImpl>,
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
        let scheduler_config = SchedulerConfig {
            thread_count: config.thread_count,
        };
        let native_registry = Arc::new(BifRegistryImpl::new());
        let scheduler = Scheduler::with_code_server(
            scheduler_config,
            Arc::clone(&module_registry),
            Arc::clone(&atom_table),
            Arc::clone(&native_registry),
        )
        .map_err(runtime_error_from_display)?;

        Ok(Self {
            scheduler,
            atom_table,
            module_registry,
            native_registry,
        })
    }

    /// Install collected NIF entries into beamr's native registry.
    ///
    /// Consumes the registration collection so no caller can append more entries
    /// after this installation step. Callers must invoke this before loading and
    /// spawning workflow modules whose imports depend on these NIFs.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::NifRegistration`] when beamr rejects an entry,
    /// including duplicate module/function/arity registrations.
    pub fn install_nifs(&self, registration: NifRegistration) -> Result<(), EngineError> {
        for entry in registration.into_entries() {
            let mfa = entry.mfa;
            let module = self.atom_table.intern(&mfa.module);
            let function = self.atom_table.intern(&mfa.function);
            let result = if entry.is_dirty {
                self.native_registry.register_dirty_shared(
                    module,
                    function,
                    mfa.arity,
                    entry.function,
                )
            } else {
                self.native_registry
                    .register_shared(module, function, mfa.arity, entry.function)
            };
            result.map_err(|error| nif_registration_error(&mfa, error))?;
        }

        Ok(())
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
    /// Returns [`EngineError::Runtime`] when beamr cannot load the module bytes,
    /// cannot resolve the loaded module name, or the loaded name does not match
    /// `deployed_name`.
    pub fn register_module(
        &self,
        deployed_name: &str,
        beam_bytes: &[u8],
    ) -> Result<(), EngineError> {
        let loaded = self
            .scheduler
            .hot_load_module(beam_bytes)
            .map_err(runtime_error_from_display)?;
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

    #[cfg(test)]
    fn lookup_native_for_test(
        &self,
        module: &str,
        function: &str,
        arity: u8,
    ) -> Option<beamr::native::NativeEntry> {
        let module = self.atom_table.intern(module);
        let function = self.atom_table.intern(function);
        self.native_registry.lookup(module, function, arity)
    }
}

fn runtime_error(reason: String) -> EngineError {
    EngineError::Runtime { reason }
}

fn runtime_error_from_display(reason: impl std::fmt::Display) -> EngineError {
    runtime_error(reason.to_string())
}

fn nif_registration_error(mfa: &Mfa, error: NativeRegistrationError) -> EngineError {
    match error {
        NativeRegistrationError::DuplicateMfa { .. } => EngineError::NifRegistration {
            reason: format!("native function already registered for {}", mfa.display()),
        },
    }
}

#[cfg(test)]
mod tests {
    use beamr::native::ProcessContext;
    use beamr::term::Term;

    use super::{RuntimeHandle, RuntimeInput};
    use crate::error::EngineError;
    use crate::runtime::{Mfa, NifEntry, NifRegistration, RuntimeConfig};

    fn forty_two(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        Ok(Term::small_int(42))
    }

    fn thirteen(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        Ok(Term::small_int(13))
    }

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

    #[test]
    fn duplicate_nif_mfa_returns_typed_error() -> Result<(), Box<dyn std::error::Error>> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
        let mfa = Mfa::new("host", "answer", 0);
        let mut registration = NifRegistration::new();
        registration.add_host_nifs([
            NifEntry::new(mfa.clone(), forty_two),
            NifEntry::dirty(mfa, thirteen),
        ]);

        let error = runtime.install_nifs(registration).err();

        assert!(matches!(
            error,
            Some(EngineError::NifRegistration { reason })
                if reason.contains("host:answer/0")
        ));
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn distinct_nifs_are_registered_and_callable() -> Result<(), Box<dyn std::error::Error>> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
        let mut registration = NifRegistration::new();
        registration.add_host_nifs([
            NifEntry::new(Mfa::new("host", "answer", 0), forty_two),
            NifEntry::dirty(Mfa::new("host", "thirteen", 0), thirteen),
        ]);

        runtime.install_nifs(registration)?;

        let answer = runtime.lookup_native_for_test("host", "answer", 0);
        let thirteen_entry = runtime.lookup_native_for_test("host", "thirteen", 0);
        let mut context = ProcessContext::new();
        assert_eq!(
            answer.map(|entry| (entry.function)(&[], &mut context)),
            Some(Ok(Term::small_int(42)))
        );
        assert!(thirteen_entry.is_some_and(|entry| entry.is_dirty));
        runtime.shutdown()?;
        Ok(())
    }
}
