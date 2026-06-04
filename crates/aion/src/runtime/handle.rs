//! `RuntimeHandle` spawn, register, cancel, and shutdown support.

use std::sync::Arc;

use aion_core::{ActivityError, ActivityErrorKind, Payload};
use beamr::atom::AtomTable;
use beamr::loader::prepare_module;
use beamr::module::ModuleRegistry;
use beamr::native::{BifRegistryImpl, NativeRegistrationError};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

use crate::error::EngineError;

use super::config::RuntimeConfig;
use super::nif::{Mfa, NifRegistration};
use super::payload::{payload_to_term, term_to_payload};

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
    /// Convert one durable payload into the single BEAM argument used by
    /// in-VM activity dispatch.
    ///
    /// The runtime boundary owns this representation. JSON primitives map to
    /// immediate terms where possible; unsupported structured input is passed as
    /// `nil` until richer payload term support lands in beamr.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when a JSON number does not fit in an
    /// immediate small integer.
    pub fn from_payload(payload: &Payload) -> Result<Self, EngineError> {
        Ok(Self {
            terms: vec![payload_to_term(payload)?],
        })
    }

    /// Number of terms supplied to the BEAM entrypoint.
    #[must_use]
    pub fn arity(&self) -> u8 {
        u8::try_from(self.terms.len()).unwrap_or(u8::MAX)
    }

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
    activity_results: Arc<dashmap::DashMap<(Pid, Pid), Payload>>,
    activity_errors: Arc<dashmap::DashMap<(Pid, Pid), ActivityError>>,
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
            activity_results: Arc::new(dashmap::DashMap::new()),
            activity_errors: Arc::new(dashmap::DashMap::new()),
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

    /// Spawn a top-level workflow process with trap-exit enabled before it runs.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the module/function/arity cannot be
    /// resolved or beamr rejects the spawn request.
    pub fn spawn_workflow_trapping(
        &self,
        deployed_module: &str,
        function: &str,
        input: RuntimeInput,
    ) -> Result<Pid, EngineError> {
        let module = self.atom_table.intern(deployed_module);
        let function = self.atom_table.intern(function);
        self.scheduler
            .spawn_trap_exit(module, function, input.into_terms())
            .map_err(runtime_error_from_display)
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
        let arity = input.arity();
        let module = self.atom_table.intern(deployed_module);
        let function_atom = self.atom_table.intern(function);
        if self.is_dirty_with_arity(deployed_module, function, arity) {
            self.scheduler
                .spawn_link_dirty(parent_pid, module, function_atom, input.into_terms())
                .map_err(runtime_error_from_display)
        } else {
            self.scheduler
                .spawn_link(parent_pid, module, function_atom, input.into_terms())
                .map_err(runtime_error_from_display)
        }
    }

    /// Return whether the registered native activity entry is dirty for arity 1.
    #[must_use]
    pub fn is_dirty(&self, module: &str, function: &str) -> bool {
        self.is_dirty_with_arity(module, function, 1)
    }

    /// Return whether the registered native entry is dirty for the supplied arity.
    #[must_use]
    pub fn is_dirty_with_arity(&self, module: &str, function: &str, arity: u8) -> bool {
        let module = self.atom_table.intern(module);
        let function = self.atom_table.intern(function);
        self.native_registry
            .lookup(module, function, arity)
            .is_some_and(|entry| entry.is_dirty)
    }

    /// Block until an activity exits, then surface its success or failure to the parent.
    ///
    /// Normal returns become typed payload results queued for the workflow and
    /// abnormal exits become typed activity errors that can be read alongside the
    /// trapped EXIT message delivered by the runtime link.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the parent is not live, the result
    /// term cannot be converted to a payload, or mailbox delivery fails.
    pub fn propagate_activity_outcome(
        &self,
        parent_pid: Pid,
        activity_pid: Pid,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(parent_pid)?;
        let (reason, result) = self.scheduler.run_until_exit(activity_pid);
        if reason == ExitReason::Normal {
            let payload = term_to_payload(result, &self.atom_table)?;
            self.deliver_activity_result(parent_pid, activity_pid, payload)
        } else {
            let error = self
                .activity_errors
                .get(&(parent_pid, activity_pid))
                .map_or_else(
                    || ActivityError {
                        kind: ActivityErrorKind::Terminal,
                        message: format!("activity process {activity_pid} exited: {reason:?}"),
                        details: None,
                    },
                    |entry| entry.clone(),
                );
            self.deliver_activity_error(parent_pid, activity_pid, error)
        }
    }

    /// Deliver a successful activity result payload to the workflow mailbox surface.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// mailbox marker cannot be queued.
    pub fn deliver_activity_result(
        &self,
        parent_pid: Pid,
        activity_pid: Pid,
        payload: Payload,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(parent_pid)?;
        self.activity_results
            .insert((parent_pid, activity_pid), payload);
        let marker = self.atom_table.intern("aion_activity_result");
        if self.scheduler.enqueue_atom_message(parent_pid, marker) {
            Ok(())
        } else {
            Err(runtime_error(format!(
                "failed to deliver activity result from {activity_pid} to {parent_pid}"
            )))
        }
    }

    /// Store a typed activity error for a trapped activity EXIT signal.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow process is not live.
    pub fn deliver_activity_error(
        &self,
        parent_pid: Pid,
        activity_pid: Pid,
        error: ActivityError,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(parent_pid)?;
        self.activity_errors
            .insert((parent_pid, activity_pid), error);
        Ok(())
    }

    /// Read a previously delivered activity result payload.
    #[must_use]
    pub fn activity_result(&self, parent_pid: Pid, activity_pid: Pid) -> Option<Payload> {
        self.activity_results
            .get(&(parent_pid, activity_pid))
            .map(|entry| entry.clone())
    }

    /// Read a previously delivered activity error associated with a trapped exit.
    #[must_use]
    pub fn activity_error(&self, parent_pid: Pid, activity_pid: Pid) -> Option<ActivityError> {
        self.activity_errors
            .get(&(parent_pid, activity_pid))
            .map(|entry| entry.clone())
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
            self.native_registry.as_ref(),
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

    /// Set a live process' trap-exit flag, returning the previous value.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when `pid` is not live.
    pub fn set_trap_exit(&self, pid: Pid, value: bool) -> Result<bool, EngineError> {
        self.scheduler
            .set_trap_exit(pid, value)
            .map_err(runtime_error_from_display)
    }

    /// Return true when `pid` is currently live.
    #[must_use]
    pub fn is_live(&self, pid: Pid) -> bool {
        self.scheduler.process_table().get(pid).is_some()
    }

    /// Return a live process' trap-exit flag.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when `pid` is not live.
    pub fn trap_exit(&self, pid: Pid) -> Result<bool, EngineError> {
        self.scheduler
            .trap_exit(pid)
            .ok_or_else(|| runtime_error(format!("process {pid} is not live")))
    }

    /// Return true when two live processes have a bidirectional link.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when either process is not live.
    pub fn is_linked(&self, left: Pid, right: Pid) -> Result<bool, EngineError> {
        self.ensure_live_pid(left)?;
        self.ensure_live_pid(right)?;
        Ok(self.scheduler.is_linked(left, right))
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

    /// Spawn an inert test process without module code.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when beamr rejects the test spawn.
    #[cfg(test)]
    pub fn spawn_test_process(&self) -> Result<Pid, EngineError> {
        Ok(self.scheduler.spawn_test_process(false))
    }

    /// Spawn an inert test process with explicit trap-exit state.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when beamr rejects the test spawn.
    #[cfg(test)]
    pub fn spawn_test_process_with_trap_exit(&self, trap_exit: bool) -> Result<Pid, EngineError> {
        Ok(self.scheduler.spawn_test_process(trap_exit))
    }

    /// Spawn an inert linked test child without enabling trap-exit on the child.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the parent is not live or beamr
    /// rejects the linked spawn.
    #[cfg(test)]
    pub fn spawn_linked_test_process(&self, parent_pid: Pid) -> Result<Pid, EngineError> {
        self.ensure_live_pid(parent_pid)?;
        self.scheduler
            .spawn_linked_test_process(parent_pid)
            .map_err(runtime_error_from_display)
    }

    /// Return true when a live process has a trapped EXIT message from `source_pid`.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when `target_pid` is not live.
    #[cfg(test)]
    pub fn has_trapped_exit_message(
        &self,
        target_pid: Pid,
        source_pid: Pid,
    ) -> Result<bool, EngineError> {
        self.ensure_live_pid(target_pid)?;
        Ok(self
            .scheduler
            .has_trapped_exit_message(target_pid, source_pid)
            .unwrap_or(false))
    }

    /// Terminate a test process with a trappable abnormal reason.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when `pid` is not live.
    #[cfg(test)]
    pub fn terminate_test_process_with_error(&self, pid: Pid) -> Result<(), EngineError> {
        self.ensure_live_pid(pid)?;
        self.scheduler.terminate_process(pid, ExitReason::Error);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn lookup_native_for_test(
        &self,
        module: &str,
        function: &str,
        arity: u8,
    ) -> Option<beamr::native::NativeEntry> {
        let module = self.atom_table.intern(module);
        let function = self.atom_table.intern(function);
        self.native_registry.lookup(module, function, arity)
    }

    #[cfg(test)]
    fn run_until_exit_for_test(&self, pid: Pid) -> (ExitReason, Term) {
        self.scheduler.run_until_exit(pid)
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
#[path = "handle/test_support.rs"]
mod test_support;

#[cfg(test)]
mod tests {
    use beamr::loader::Instruction;
    use beamr::loader::decode::compact::Operand;
    use beamr::module::{Module, ResolvedImport, ResolvedImportTarget};
    use beamr::native::ProcessContext;
    use beamr::term::Term;

    use super::{RuntimeHandle, RuntimeInput};
    use crate::error::EngineError;
    use crate::runtime::{Mfa, NifEntry, NifRegistration, RuntimeConfig};

    fn forty_two(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        if args.len() > 255 {
            return Err(Term::small_int(0));
        }
        Ok(Term::small_int(42))
    }

    fn thirteen(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        if args.len() > 255 {
            return Err(Term::small_int(0));
        }
        Ok(Term::small_int(13))
    }

    fn native_call_module_for_test(
        module: beamr::atom::Atom,
        function: beamr::atom::Atom,
        target_module: beamr::atom::Atom,
        target_function: beamr::atom::Atom,
        native_entry: Option<beamr::native::NativeEntry>,
    ) -> Module {
        let label = 1;
        let code = vec![
            Instruction::Label { label },
            Instruction::CallExt {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ];
        let mut module_data = Module {
            name: module,
            generation: 0,
            exports: std::collections::HashMap::from([((function, 0), label)]),
            label_index: std::collections::HashMap::from([(label, 0)]),
            code,
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        };
        if let Some(native_entry) = native_entry {
            module_data.resolved_imports.push(ResolvedImport {
                module: target_module,
                function: target_function,
                arity: 0,
                target: ResolvedImportTarget::Native(native_entry),
            });
        }
        module_data
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
        registration.add_engine_nifs().add_host_nifs([
            NifEntry::new(Mfa::new("host", "answer", 0), forty_two),
            NifEntry::dirty(Mfa::new("host", "thirteen", 0), thirteen),
        ]);

        runtime.install_nifs(registration)?;

        let answer = runtime.lookup_native_for_test("host", "answer", 0);
        assert!(answer.is_some());
        assert!(
            runtime
                .lookup_native_for_test("host", "thirteen", 0)
                .is_some_and(|entry| entry.is_dirty)
        );

        let host_nif_call = native_call_module_for_test(
            runtime.atom_table.intern("host_nif_call"),
            runtime.atom_table.intern("answer"),
            runtime.atom_table.intern("host"),
            runtime.atom_table.intern("answer"),
            answer,
        );
        runtime.module_registry.insert(host_nif_call);
        let pid = runtime.spawn_workflow("host_nif_call", "answer", RuntimeInput::default())?;
        let (reason, result) = runtime.run_until_exit_for_test(pid);

        assert_eq!(reason, beamr::process::ExitReason::Normal);
        assert_eq!(result, Term::small_int(42));
        runtime.shutdown()?;
        Ok(())
    }
}
