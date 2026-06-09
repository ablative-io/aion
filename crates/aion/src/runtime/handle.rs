//! `RuntimeHandle` spawn, register, cancel, and shutdown support.

use std::sync::{Arc, Mutex};

use aion_core::{ActivityError, ActivityErrorKind, ContentType, Payload};
use beamr::atom::{Atom, AtomTable};
use beamr::module::ModuleRegistry;
use beamr::native::{BifRegistryImpl, NativeRegistrationError};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

use crate::error::EngineError;

use super::config::{RuntimeConfig, SignalDeliveryConfig};
use super::nif::{Mfa, NifRegistration};
use super::payload::{payload_to_term, term_to_payload};

/// Local BEAM process identifier exposed by the runtime boundary.
pub type Pid = u64;

type RetainedHeap = Box<[u64]>;
type RetainedHeaps = Vec<RetainedHeap>;
type RetainedSpawnHeaps = Arc<dashmap::DashMap<Pid, Mutex<RetainedHeaps>>>;

/// Runtime-owned workflow or activity input terms.
///
/// The wrapper keeps the beamr term representation inside the runtime module
/// while later lifecycle and payload code decide how durable payloads become VM
/// terms.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct RuntimeInput {
    terms: Vec<Term>,
    heaps: RetainedHeaps,
}

impl RuntimeInput {
    /// Convert one durable payload into the single BEAM argument used by
    /// in-VM activity dispatch.
    ///
    /// The runtime boundary owns this representation. JSON payloads are passed
    /// as BEAM binary terms and any boxed host heap backing those terms is
    /// retained until the spawned process is observed exiting or cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when a JSON number does not fit in an
    /// immediate small integer.
    pub fn from_payload(payload: &Payload) -> Result<Self, EngineError> {
        let (term, heaps) = payload_to_term(payload)?.into_parts();
        Ok(Self {
            terms: vec![term],
            heaps,
        })
    }

    /// Number of terms supplied to the BEAM entrypoint.
    #[must_use]
    pub fn arity(&self) -> u8 {
        u8::try_from(self.terms.len()).unwrap_or(u8::MAX)
    }

    fn into_spawn_parts(self) -> (Vec<Term>, RetainedHeaps) {
        (self.terms, self.heaps)
    }
}

/// Handle to the embedded beamr scheduler and code-server state.
pub struct RuntimeHandle {
    pub(super) scheduler: Scheduler,
    pub(super) atom_table: Arc<AtomTable>,
    pub(super) module_registry: Arc<ModuleRegistry>,
    pub(super) native_registry: Arc<BifRegistryImpl>,
    activity_results: Arc<dashmap::DashMap<(Pid, Pid), Payload>>,
    activity_errors: Arc<dashmap::DashMap<(Pid, Pid), ActivityError>>,
    signal_messages: Arc<dashmap::DashMap<Pid, Vec<(String, Payload)>>>,
    registered_nif_modules: Arc<dashmap::DashSet<String>>,
    spawn_heaps: RetainedSpawnHeaps,
    signal_delivery: SignalDeliveryConfig,
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
        register_all_bifs(&native_registry, &atom_table)?;
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
            signal_messages: Arc::new(dashmap::DashMap::new()),
            registered_nif_modules: Arc::new(dashmap::DashSet::new()),
            spawn_heaps: Arc::new(dashmap::DashMap::new()),
            signal_delivery: config.signal_delivery,
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
            let capability = beamr::native::Capability::ExternalIo;
            let result = if entry.is_dirty {
                self.native_registry.register_dirty(
                    module,
                    function,
                    mfa.arity,
                    entry.function,
                    capability,
                )
            } else {
                self.native_registry.register(
                    module,
                    function,
                    mfa.arity,
                    entry.function,
                    capability,
                )
            };
            result.map_err(|error| nif_registration_error(&mfa, error))?;
            self.registered_nif_modules.insert(mfa.module);
        }

        Ok(())
    }

    /// Return module names that have registered NIFs and should not be
    /// content-hash renamed during package loading.
    #[must_use]
    pub fn registered_nif_modules(&self) -> Vec<String> {
        let mut module_names: Vec<_> = self
            .registered_nif_modules
            .iter()
            .map(|module_name| module_name.key().clone())
            .collect();
        module_names.sort();
        module_names
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
        self.release_dead_spawn_heaps();
        let module = self.atom_table.intern(deployed_module);
        let function = self.atom_table.intern(function);
        let (terms, heaps) = input.into_spawn_parts();
        let pid = self
            .scheduler
            .spawn_trap_exit(module, function, terms)
            .map_err(runtime_error_from_display)?;
        self.retain_spawn_heaps(pid, heaps);
        Ok(pid)
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
        self.release_dead_spawn_heaps();
        self.ensure_live_pid(parent_pid)?;
        self.wait_for_process_ready(parent_pid)?;
        let arity = input.arity();
        let module = self.atom_table.intern(deployed_module);
        let function_atom = self.atom_table.intern(function);
        let (terms, heaps) = input.into_spawn_parts();
        let pid = if self.is_dirty_with_arity(deployed_module, function, arity) {
            self.scheduler
                .spawn_link_dirty(parent_pid, module, function_atom, terms)
                .map_err(runtime_error_from_display)?
        } else {
            self.scheduler
                .spawn_link(parent_pid, module, function_atom, terms)
                .map_err(runtime_error_from_display)?
        };
        self.retain_spawn_heaps(pid, heaps);
        Ok(pid)
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
        self.release_spawn_heaps(activity_pid);
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

    /// Deliver a recorded signal marker to the workflow mailbox surface.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// mailbox marker cannot be queued.
    pub fn deliver_signal_received(
        &self,
        workflow_pid: Pid,
        name: String,
        payload: Payload,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        self.wait_for_process_ready(workflow_pid)?;
        let mut messages = self.signal_messages.entry(workflow_pid).or_default();
        messages.push((name, payload));
        let marker = self.atom_table.intern("aion_signal_received");
        match self.enqueue_signal_marker_with_retry(workflow_pid, marker) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = messages.pop();
                Err(error)
            }
        }
    }

    /// Deliver a two-phase activity completion marker to the workflow mailbox.
    ///
    /// The structured `{activity_complete, CorrelationId, Result}` payload is
    /// retained in the runtime boundary, and an atom marker wakes any suspended
    /// selective receive. The await NIF resolves the retained payload by
    /// correlation id after consuming the marker.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// marker cannot be queued.
    pub(crate) fn deliver_activity_completion_message(
        &self,
        workflow_pid: Pid,
        correlation_id: String,
        result: String,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        let activity_id = correlation_to_activity_pid(&correlation_id)?;
        self.activity_results.insert(
            (workflow_pid, activity_id),
            Payload::new(ContentType::Json, result.into_bytes()),
        );
        let marker = self.atom_table.intern("activity_complete");
        self.enqueue_activity_marker(workflow_pid, marker, &correlation_id)
    }

    /// Deliver a two-phase activity failure marker to the workflow mailbox.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the workflow is not live or the
    /// marker cannot be queued.
    pub(crate) fn deliver_activity_failure_message(
        &self,
        workflow_pid: Pid,
        correlation_id: String,
        reason: String,
    ) -> Result<(), EngineError> {
        self.ensure_live_pid(workflow_pid)?;
        let activity_id = correlation_to_activity_pid(&correlation_id)?;
        self.activity_errors
            .insert((workflow_pid, activity_id), activity_error(reason));
        let marker = self.atom_table.intern("activity_failed");
        self.enqueue_activity_marker(workflow_pid, marker, &correlation_id)
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

    fn enqueue_activity_marker(
        &self,
        workflow_pid: Pid,
        marker: Atom,
        correlation_id: &str,
    ) -> Result<(), EngineError> {
        if self.scheduler.enqueue_atom_message(workflow_pid, marker) {
            tracing::debug!(
                workflow_pid,
                correlation_id,
                "delivered activity completion marker to workflow mailbox via scheduler queue"
            );
            Ok(())
        } else {
            Err(runtime_error(format!(
                "failed to deliver activity completion marker {correlation_id} to {workflow_pid}"
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

    /// Read delivered signal messages retained for the workflow process.
    #[must_use]
    pub fn signal_messages(&self, workflow_pid: Pid) -> Vec<(String, Payload)> {
        self.signal_messages
            .get(&workflow_pid)
            .map_or_else(Vec::new, |entry| entry.clone())
    }

    /// Remove and return the oldest retained signal with `name` for the workflow process.
    #[must_use]
    pub fn take_signal_message(&self, workflow_pid: Pid, name: &str) -> Option<Payload> {
        let mut messages = self.signal_messages.get_mut(&workflow_pid)?;
        let index = messages
            .iter()
            .position(|(message_name, _)| message_name == name)?;
        Some(messages.remove(index).1)
    }

    /// Wait until a delivered signal with `name` is retained for the workflow process.
    pub fn wait_for_signal_message(&self, workflow_pid: Pid, name: &str) -> Payload {
        loop {
            if let Some(payload) = self.take_signal_message(workflow_pid, name) {
                return payload;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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

    pub(crate) fn take_activity_result(
        &self,
        parent_pid: Pid,
        activity_sequence: Pid,
    ) -> Option<Payload> {
        self.activity_results
            .remove(&(parent_pid, activity_sequence))
            .map(|(_, payload)| payload)
    }

    pub(crate) fn take_activity_error(
        &self,
        parent_pid: Pid,
        activity_sequence: Pid,
    ) -> Option<ActivityError> {
        self.activity_errors
            .remove(&(parent_pid, activity_sequence))
            .map(|(_, error)| error)
    }

    pub(crate) fn activity_complete_atom(&self) -> Atom {
        self.atom_table.intern("activity_complete")
    }

    pub(crate) fn activity_failed_atom(&self) -> Atom {
        self.atom_table.intern("activity_failed")
    }

    /// Cancel a live process by PID.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when `pid` is not live.
    pub fn cancel_pid(&self, pid: Pid) -> Result<(), EngineError> {
        self.ensure_live_pid(pid)?;
        self.scheduler.terminate_process(pid, ExitReason::Kill);
        self.release_spawn_heaps(pid);
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
        self.spawn_heaps.clear();
        Ok(())
    }

    fn spawn_process(
        &self,
        deployed_module: &str,
        function: &str,
        input: RuntimeInput,
    ) -> Result<Pid, EngineError> {
        self.release_dead_spawn_heaps();
        let module = self.atom_table.intern(deployed_module);
        let function = self.atom_table.intern(function);
        let (terms, heaps) = input.into_spawn_parts();
        let pid = self
            .scheduler
            .spawn(module, function, terms)
            .map_err(runtime_error_from_display)?;
        self.retain_spawn_heaps(pid, heaps);
        Ok(pid)
    }

    fn retain_spawn_heaps(&self, pid: Pid, heaps: RetainedHeaps) {
        if heaps.is_empty() {
            return;
        }
        self.spawn_heaps.insert(pid, Mutex::new(heaps));
    }

    pub(super) fn release_spawn_heaps(&self, pid: Pid) {
        self.spawn_heaps.remove(&pid);
    }

    fn release_dead_spawn_heaps(&self) {
        let dead_pids: Vec<Pid> = self
            .spawn_heaps
            .iter()
            .filter_map(|entry| {
                let pid = *entry.key();
                self.scheduler
                    .process_table()
                    .get(pid)
                    .is_none()
                    .then_some(pid)
            })
            .collect();
        for pid in dead_pids {
            self.release_spawn_heaps(pid);
        }
    }

    pub(super) fn ensure_live_pid(&self, pid: Pid) -> Result<(), EngineError> {
        if self.scheduler.process_table().get(pid).is_some() {
            Ok(())
        } else {
            Err(runtime_error(format!("process {pid} is not live")))
        }
    }

    pub(crate) fn wait_for_process_ready(&self, pid: Pid) -> Result<(), EngineError> {
        let deadline = std::time::Instant::now() + self.signal_delivery.ready_timeout;
        while std::time::Instant::now() < deadline {
            if self.scheduler.trap_exit(pid).is_some() {
                return Ok(());
            }
            sleep_signal_delivery_backoff(self.signal_delivery.initial_backoff);
        }
        self.scheduler
            .trap_exit(pid)
            .map(|_| ())
            .ok_or_else(|| runtime_error(format!("process {pid} is not ready")))
    }

    fn enqueue_signal_marker_with_retry(
        &self,
        workflow_pid: Pid,
        marker: Atom,
    ) -> Result<(), EngineError> {
        let attempts = self.signal_delivery.max_enqueue_attempts.max(1);
        let mut backoff = self.signal_delivery.initial_backoff;
        for attempt in 1..=attempts {
            if self.scheduler.enqueue_atom_message(workflow_pid, marker) {
                return Ok(());
            }

            if self.scheduler.process_table().get(workflow_pid).is_none() {
                return Err(runtime_error(format!(
                    "failed to deliver signal to workflow process {workflow_pid}: process is not live"
                )));
            }

            if attempt < attempts {
                // beamr 0.3.15 normal spawn publishes the PID before a scheduler
                // worker materializes the process body from its SpawnRequest. It
                // also exposes an Executing slot while the process is running.
                // enqueue_atom_message only accepts a Present slot, so an alive
                // just-spawned or currently executing process can transiently
                // return false even after the liveness/ready gate above.
                sleep_signal_delivery_backoff(backoff);
                backoff = next_signal_delivery_backoff(backoff, self.signal_delivery.max_backoff);
            }
        }

        Err(runtime_error(format!(
            "failed to deliver signal to workflow process {workflow_pid} after {attempts} attempts"
        )))
    }

    /// Register a test module whose exported function waits indefinitely.
    ///
    /// This keeps lifecycle tests at the runtime boundary while still exercising
    /// real module lookup and trap-exit workflow spawning.
    #[cfg(test)]
    pub fn register_waiting_test_module(&self, deployed_name: &str, function: &str) {
        use std::collections::HashMap;

        use beamr::loader::Instruction;
        use beamr::loader::decode::compact::Operand;
        use beamr::module::Module;

        let module = self.atom_table.intern(deployed_name);
        let function = self.atom_table.intern(function);
        let label = 10;
        self.module_registry.insert(Module {
            name: module,
            generation: 0,
            exports: HashMap::from([((function, 1), label)]),
            label_index: HashMap::from([(label, 0)]),
            code: vec![
                Instruction::Label { label },
                Instruction::Wait {
                    fail: Operand::Label(label),
                },
            ],
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        });
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

    /// Poll until a trapped EXIT message from `source_pid` arrives at `target_pid`.
    ///
    /// beamr delivers exit signals asynchronously after process termination.
    /// Tests that assert on trapped exit messages must wait for delivery.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] if the message does not arrive within 50ms.
    #[cfg(test)]
    pub fn wait_for_trapped_exit(
        &self,
        target_pid: Pid,
        source_pid: Pid,
    ) -> Result<(), EngineError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        while std::time::Instant::now() < deadline {
            if self
                .scheduler
                .has_trapped_exit_message(target_pid, source_pid)
                .unwrap_or(false)
            {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Err(runtime_error(format!(
            "trapped exit from {source_pid} to {target_pid} did not arrive"
        )))
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
    pub(crate) fn run_until_exit_for_test(&self, pid: Pid) -> (ExitReason, Term) {
        let result = self.scheduler.run_until_exit(pid);
        self.release_spawn_heaps(pid);
        result
    }

    #[cfg(test)]
    pub(crate) fn retained_spawn_heap_count_for_test(&self) -> usize {
        self.release_dead_spawn_heaps();
        self.spawn_heaps.len()
    }
}

fn runtime_error(reason: String) -> EngineError {
    EngineError::Runtime { reason }
}

fn activity_error(message: String) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message,
        details: None,
    }
}

fn correlation_to_activity_pid(correlation_id: &str) -> Result<Pid, EngineError> {
    let Some(raw) = correlation_id.strip_prefix("activity:") else {
        return Err(runtime_error(format!(
            "invalid activity correlation id {correlation_id}"
        )));
    };
    raw.parse::<Pid>().map_err(|error| {
        runtime_error(format!(
            "invalid activity correlation sequence {correlation_id}: {error}"
        ))
    })
}

fn next_signal_delivery_backoff(
    current: std::time::Duration,
    max: std::time::Duration,
) -> std::time::Duration {
    let doubled = current.saturating_mul(2);
    if doubled > max { max } else { doubled }
}

fn sleep_signal_delivery_backoff(duration: std::time::Duration) {
    if duration.is_zero() {
        std::thread::yield_now();
    } else {
        std::thread::sleep(duration);
    }
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

fn register_all_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), EngineError> {
    use beamr::native::{
        bifs::register_gate1_bifs, gate3_bifs::register_gate3_bifs,
        gleam_ffi::register_gleam_ffi_bifs, otp_stubs::init_otp_atoms,
        otp_stubs::register_otp_stubs, process_bifs::register_gate2_bifs,
        selector_ffi::register_selector_bifs, stdlib_stubs::register_stdlib_stubs,
    };
    register_gate1_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_gate2_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_gate3_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_stdlib_stubs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_selector_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_gleam_ffi_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    init_otp_atoms(atom_table);
    register_otp_stubs(registry, atom_table).map_err(runtime_error_from_display)?;
    Ok(())
}

#[cfg(test)]
#[path = "handle/test_support.rs"]
mod test_support;

#[cfg(test)]
mod tests {
    use aion_core::Payload;
    use std::time::Duration;

    use beamr::loader::Instruction;
    use beamr::loader::decode::compact::Operand;
    use beamr::module::{Module, ResolvedImport, ResolvedImportTarget};
    use beamr::native::ProcessContext;
    use beamr::term::Term;
    use beamr::term::binary::Binary;

    use super::{RuntimeHandle, RuntimeInput};
    use crate::error::EngineError;
    use crate::runtime::{Mfa, NifEntry, NifRegistration, RuntimeConfig, SignalDeliveryConfig};

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

    fn binary_length(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        match args {
            [term] => Binary::new(*term)
                .and_then(|binary| i64::try_from(binary.as_bytes().len()).ok())
                .map(Term::small_int)
                .ok_or_else(|| Term::small_int(0)),
            _ => Err(Term::small_int(0)),
        }
    }

    fn native_call_module_for_test(
        module: beamr::atom::Atom,
        function: beamr::atom::Atom,
        target_module: beamr::atom::Atom,
        target_function: beamr::atom::Atom,
        native_entry: Option<beamr::native::NativeEntry>,
    ) -> Module {
        native_call_module_with_arity_for_test(
            module,
            function,
            target_module,
            target_function,
            0,
            native_entry,
        )
    }

    fn native_call_module_with_arity_for_test(
        module: beamr::atom::Atom,
        function: beamr::atom::Atom,
        target_module: beamr::atom::Atom,
        target_function: beamr::atom::Atom,
        arity: u8,
        native_entry: Option<beamr::native::NativeEntry>,
    ) -> Module {
        let label = 1;
        let code = vec![
            Instruction::Label { label },
            Instruction::CallExt {
                arity: Operand::Unsigned(arity.into()),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ];
        let mut module_data = Module {
            name: module,
            generation: 0,
            exports: std::collections::HashMap::from([((function, arity), label)]),
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
                arity,
                target: ResolvedImportTarget::Native(native_entry),
            });
        }
        module_data
    }

    fn assert_send_sync<T: Send + Sync>() {}

    fn fixture_workflow_beam() -> &'static [u8] {
        include_bytes!("../../tests/fixtures/aion_fixture_workflow.beam")
    }

    #[test]
    fn runtime_handle_is_send_sync() {
        assert_send_sync::<RuntimeHandle>();
    }

    #[test]
    fn registers_spawns_and_shuts_down() -> Result<(), Box<dyn std::error::Error>> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
        runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;

        let pid =
            runtime.spawn_workflow("aion_fixture_workflow", "wait", RuntimeInput::default())?;
        assert!(runtime.cancel_pid(pid).is_ok());
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn signal_delivery_failure_rolls_back_retained_message()
    -> Result<(), Box<dyn std::error::Error>> {
        let signal_delivery =
            SignalDeliveryConfig::new(Duration::ZERO, 1, Duration::ZERO, Duration::ZERO);
        let runtime =
            RuntimeHandle::new(RuntimeConfig::new(Some(1)).with_signal_delivery(signal_delivery))?;
        let pid = runtime.spawn_test_process()?;
        runtime.terminate_test_process_with_error(pid)?;

        let error = runtime
            .deliver_signal_received(
                pid,
                "wake".to_owned(),
                Payload::from_json(&serde_json::json!(true))?,
            )
            .err()
            .ok_or("dead process delivery unexpectedly succeeded")?;

        assert!(matches!(error, EngineError::Runtime { .. }));
        assert!(runtime.signal_messages(pid).is_empty());
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
        assert_eq!(runtime.registered_nif_modules(), vec!["host"]);
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn payload_binary_remains_valid_through_spawn_and_is_released()
    -> Result<(), Box<dyn std::error::Error>> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
        let mfa = Mfa::new("host", "binary_length", 1);
        let mut registration = NifRegistration::new();
        registration.add_host_nifs([NifEntry::new(mfa, binary_length)]);
        runtime.install_nifs(registration)?;

        let native_entry = runtime.lookup_native_for_test("host", "binary_length", 1);
        let module = native_call_module_with_arity_for_test(
            runtime.atom_table.intern("payload_echo"),
            runtime.atom_table.intern("run"),
            runtime.atom_table.intern("host"),
            runtime.atom_table.intern("binary_length"),
            1,
            native_entry,
        );
        runtime.module_registry.insert(module);
        let payload = Payload::new(
            aion_core::ContentType::Json,
            br#"{"hello":"world"}"#.to_vec(),
        );

        let pid =
            runtime.spawn_workflow("payload_echo", "run", RuntimeInput::from_payload(&payload)?)?;
        assert_eq!(runtime.retained_spawn_heap_count_for_test(), 1);
        let (reason, result) = runtime.run_until_exit_for_test(pid);

        assert_eq!(reason, beamr::process::ExitReason::Normal);
        assert_eq!(
            result.as_small_int(),
            Some(i64::try_from(payload.bytes().len()).unwrap_or(0))
        );
        assert_eq!(runtime.retained_spawn_heap_count_for_test(), 0);
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn workflow_outcome_releases_payload_heaps() -> Result<(), Box<dyn std::error::Error>> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
        let mfa = Mfa::new("host", "binary_length", 1);
        let mut registration = NifRegistration::new();
        registration.add_host_nifs([NifEntry::new(mfa, binary_length)]);
        runtime.install_nifs(registration)?;

        let native_entry = runtime.lookup_native_for_test("host", "binary_length", 1);
        let module = native_call_module_with_arity_for_test(
            runtime.atom_table.intern("payload_workflow_outcome"),
            runtime.atom_table.intern("run"),
            runtime.atom_table.intern("host"),
            runtime.atom_table.intern("binary_length"),
            1,
            native_entry,
        );
        runtime.module_registry.insert(module);
        let payload = Payload::new(
            aion_core::ContentType::Json,
            br#"{"workflow":"outcome"}"#.to_vec(),
        );

        let pid = runtime.spawn_workflow(
            "payload_workflow_outcome",
            "run",
            RuntimeInput::from_payload(&payload)?,
        )?;
        assert_eq!(runtime.retained_spawn_heap_count_for_test(), 1);
        let outcome = runtime.workflow_outcome(pid)?;

        assert_eq!(
            outcome?,
            Payload::from_json(&serde_json::json!(payload.bytes().len()))?
        );
        assert_eq!(runtime.retained_spawn_heap_count_for_test(), 0);
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn repeated_completed_payload_spawns_do_not_accumulate_retained_heaps()
    -> Result<(), Box<dyn std::error::Error>> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
        let mfa = Mfa::new("host", "binary_length", 1);
        let mut registration = NifRegistration::new();
        registration.add_host_nifs([NifEntry::new(mfa, binary_length)]);
        runtime.install_nifs(registration)?;

        let native_entry = runtime.lookup_native_for_test("host", "binary_length", 1);
        let module = native_call_module_with_arity_for_test(
            runtime.atom_table.intern("payload_echo_many"),
            runtime.atom_table.intern("run"),
            runtime.atom_table.intern("host"),
            runtime.atom_table.intern("binary_length"),
            1,
            native_entry,
        );
        runtime.module_registry.insert(module);
        let payload = Payload::new(
            aion_core::ContentType::Json,
            br#"{"iteration":true}"#.to_vec(),
        );

        for _ in 0..1_000 {
            let pid = runtime.spawn_workflow(
                "payload_echo_many",
                "run",
                RuntimeInput::from_payload(&payload)?,
            )?;
            let (reason, result) = runtime.run_until_exit_for_test(pid);
            assert_eq!(reason, beamr::process::ExitReason::Normal);
            assert_eq!(
                result.as_small_int(),
                Some(i64::try_from(payload.bytes().len()).unwrap_or(0))
            );
            assert_eq!(runtime.retained_spawn_heap_count_for_test(), 0);
        }

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

        assert_eq!(
            runtime.registered_nif_modules(),
            vec!["aion_flow_ffi", "host"]
        );
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
