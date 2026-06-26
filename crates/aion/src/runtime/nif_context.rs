//! Per-call NIF context resolution and durability replay checks.

use std::future::Future;
use std::sync::Arc;

use aion_core::{ActivityError, ActivityId, Payload, RunId, WorkflowId};
use aion_store::EventStore;
use chrono::{DateTime, Utc};
use tokio::runtime::Handle;
use tokio::sync::Mutex;

use crate::EngineError;
use crate::durability::{
    Command, DurabilityError, FanOutCompletionResult, FanOutItem, FanOutOutcome, HistoryCursor,
    Recorder, ResolveOutcome, Resolver,
};
use crate::registry::{Registry, WorkflowHandle};

/// Errors surfaced while constructing or using a per-call NIF context.
#[derive(thiserror::Error, Debug)]
pub enum NifContextError {
    /// No live workflow handle is registered for the calling process.
    #[error("unknown workflow process pid {pid}")]
    UnknownProcess {
        /// Runtime process identifier that could not be resolved.
        pid: u64,
    },
    /// The recorder lock could not be acquired.
    #[error("workflow recorder lock is poisoned")]
    RecorderPoisoned,
    /// Durability replay or recording failed.
    #[error("durability error: {0}")]
    Durability(#[from] DurabilityError),
    /// A BEAM return term could not be encoded.
    #[error("term encoding error: {reason}")]
    TermEncoding {
        /// Human-readable encoding failure reason.
        reason: String,
    },
}

impl NifContextError {
    /// NIF-convention reason string for `{error, <<reason>>}` results.
    ///
    /// Term construction lives with the callers, which allocate on the
    /// calling process heap through their [`beamr::native::ProcessContext`]
    /// (N-6); this type only renders the stable reason text.
    pub(crate) fn error_reason(&self) -> String {
        match self {
            Self::UnknownProcess { pid } => format!("unknown_process:{pid}"),
            Self::RecorderPoisoned => "recorder_poisoned".to_owned(),
            Self::Durability(error) => format!("durability:{error}"),
            Self::TermEncoding { reason } => format!("term_encoding:{reason}"),
        }
    }
}

/// Per-NIF-call context resolved from the calling runtime process.
pub struct NifContext {
    handle: WorkflowHandle,
    recorder: Arc<Mutex<Recorder>>,
    tokio_handle: Handle,
    resolver: Resolver,
    last_recorded_at: Option<DateTime<Utc>>,
}

impl NifContext {
    /// Resolves `pid` against the active registry and builds a replay resolver from recorded history.
    ///
    /// `birth_wait` bounds the registry-registration wait for a just-spawned
    /// process (see [`resolve_handle_with_birth_wait`]).
    ///
    /// # Errors
    ///
    /// Returns [`NifContextError::UnknownProcess`] when the registry has no matching active handle,
    /// or [`NifContextError::Durability`] when recorded history cannot be read or cursor-validated.
    pub fn new(
        pid: u64,
        registry: &Registry,
        tokio_handle: Handle,
        birth_wait: crate::runtime::SignalDeliveryConfig,
    ) -> Result<Self, NifContextError> {
        Self::new_with_history_store(pid, registry, tokio_handle, None, birth_wait)
    }

    /// Resolves `pid` and reads recorded history from an explicit store when supplied.
    ///
    /// If no store is supplied, the history is read through the resolved handle's recorder-owned
    /// store. The explicit store seam lets the runtime pass the engine store without exposing any
    /// mutable event-store append path to NIF code.
    ///
    /// # Errors
    ///
    /// Returns [`NifContextError::UnknownProcess`] when no active handle matches `pid`, or wraps any
    /// durability read/cursor error in [`NifContextError::Durability`].
    pub fn new_with_history_store(
        pid: u64,
        registry: &Registry,
        tokio_handle: Handle,
        store: Option<Arc<dyn EventStore>>,
        birth_wait: crate::runtime::SignalDeliveryConfig,
    ) -> Result<Self, NifContextError> {
        let handle = resolve_handle_with_birth_wait(registry, pid, birth_wait)?;
        let recorder = handle.recorder();
        let workflow_id = handle.workflow_id().clone();
        let history = match store {
            Some(store) => tokio_handle
                .block_on(store.read_history(&workflow_id))
                .map_err(DurabilityError::from)?,
            None => tokio_handle.block_on(async {
                let recorder = recorder.lock().await;
                recorder.read_history().await
            })?,
        };
        // Correlation identities (ordinals, signal occurrence indices) are
        // run-scoped; resolve only against this run's history segment.
        let history = crate::durability::current_run_segment(history, handle.run_id())?;
        let last_recorded_at = history.last().map(|event| *event.recorded_at());
        let cursor = HistoryCursor::new(history)?;
        let resolver = Resolver::new(workflow_id, cursor);

        Ok(Self {
            handle,
            recorder,
            tokio_handle,
            resolver,
            last_recorded_at,
        })
    }

    /// Returns the logical workflow identifier for the resolved handle.
    #[must_use]
    pub fn workflow_id(&self) -> &WorkflowId {
        self.handle.workflow_id()
    }

    /// Returns the concrete run identifier for the resolved handle.
    #[must_use]
    pub fn run_id(&self) -> &RunId {
        self.handle.run_id()
    }

    /// Returns the next deterministic activity key ordinal.
    ///
    /// Ordinals come from the run-scoped monotonic sequence on the workflow
    /// handle: every NIF call shares it, so successive workflow steps get
    /// unique correlation keys even though each call constructs a fresh
    /// resolver over the full history.
    #[must_use]
    pub fn next_activity_ordinal(&self) -> u64 {
        self.handle.allocate_activity_ordinals(1)
    }

    /// Allocates `count` consecutive activity key ordinals for a fan-out.
    #[must_use]
    pub fn allocate_activity_ordinals(&self, count: u64) -> u64 {
        self.handle.allocate_activity_ordinals(count)
    }

    /// Returns the next deterministic timer ordinal.
    ///
    /// Same run-scoped sequence contract as [`Self::next_activity_ordinal`];
    /// used to derive anonymous timer identities that replay deterministically.
    #[must_use]
    pub fn next_timer_ordinal(&self) -> u64 {
        self.handle.allocate_timer_ordinals(1)
    }

    /// Returns the next deterministic child-workflow spawn ordinal.
    ///
    /// Same run-scoped sequence contract as [`Self::next_activity_ordinal`]:
    /// the n-th `spawn_child` call a run makes correlates with the n-th
    /// recorded `ChildWorkflowStarted` in the run's history segment. The
    /// ordinal is never derived from the recorder's sequence head, which
    /// moves with asynchronous-arrival appends and with the resume position
    /// after recovery.
    #[must_use]
    pub fn next_child_ordinal(&self) -> u64 {
        self.handle.allocate_child_ordinals(1)
    }

    /// Number of `receive_signal(name)` calls this run has completed.
    #[must_use]
    pub fn signal_receives_consumed(&self, name: &str) -> u64 {
        self.handle.signal_receives_consumed(name)
    }

    /// Advance the completed-receive count for `name` by one.
    pub fn mark_signal_receive_consumed(&self, name: &str) {
        self.handle.mark_signal_receive_consumed(name);
    }

    /// Number of `send_signal(name)` calls this run has completed.
    #[must_use]
    pub fn signal_sends_completed(&self, name: &str) -> u64 {
        self.handle.signal_sends_completed(name)
    }

    /// Advance the completed-send count for `name` by one.
    pub fn mark_signal_send_completed(&self, name: &str) {
        self.handle.mark_signal_send_completed(name);
    }

    /// Returns a clone of the resolved workflow handle.
    #[must_use]
    pub fn workflow_handle(&self) -> WorkflowHandle {
        self.handle.clone()
    }

    /// Returns the runtime process identifier for the resolved handle.
    #[must_use]
    pub const fn pid(&self) -> u64 {
        self.handle.pid()
    }

    /// Returns the recorded timestamp of the last event in the resolved history.
    #[must_use]
    pub const fn last_recorded_at(&self) -> Option<DateTime<Utc>> {
        self.last_recorded_at
    }

    /// Returns and advances the workflow-local deterministic NIF call sequence.
    #[must_use]
    pub fn next_deterministic_sequence(&self) -> u64 {
        self.handle.next_deterministic_nif_sequence()
    }

    /// Returns the shared single-writer recorder for the resolved workflow.
    #[must_use]
    pub fn recorder(&self) -> Arc<Mutex<Recorder>> {
        Arc::clone(&self.recorder)
    }

    /// Synchronously runs an async recorder operation on the carried Tokio runtime handle.
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the supplied operation.
    pub fn block_on_recorder<T, F>(&self, f: F) -> Result<T, NifContextError>
    where
        F: for<'a> FnOnce(
            &'a mut Recorder,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<T, DurabilityError>> + Send + 'a>,
        >,
    {
        self.tokio_handle
            .block_on(async {
                let mut recorder = self.recorder.lock().await;
                f(&mut recorder).await
            })
            .map_err(Into::into)
    }

    /// Records activity scheduling and start through the workflow's single-writer recorder.
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the recorder.
    pub fn record_activity_scheduled_started(
        &self,
        recorded_at: chrono::DateTime<chrono::Utc>,
        activity_id: ActivityId,
        activity_type: String,
        input: Payload,
    ) -> Result<(), NifContextError> {
        self.tokio_handle
            .block_on(async {
                let mut recorder = self.recorder.lock().await;
                recorder
                    .record_activity_scheduled(
                        recorded_at,
                        activity_id.clone(),
                        activity_type,
                        input,
                    )
                    .await?;
                recorder
                    .record_activity_started(recorded_at, activity_id)
                    .await
            })
            .map_err(Into::into)
    }

    /// Records successful activity completion through the workflow's single-writer recorder.
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the recorder.
    pub fn record_activity_completed(
        &self,
        recorded_at: chrono::DateTime<chrono::Utc>,
        activity_id: ActivityId,
        result: Payload,
    ) -> Result<(), NifContextError> {
        self.tokio_handle
            .block_on(async {
                let mut recorder = self.recorder.lock().await;
                recorder
                    .record_activity_completed(recorded_at, activity_id, result)
                    .await
            })
            .map_err(Into::into)
    }

    /// Records terminal activity failure through the workflow's single-writer recorder.
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the recorder.
    pub fn record_activity_failed(
        &self,
        recorded_at: chrono::DateTime<chrono::Utc>,
        activity_id: ActivityId,
        error: ActivityError,
        attempt: u32,
    ) -> Result<(), NifContextError> {
        self.tokio_handle
            .block_on(async {
                let mut recorder = self.recorder.lock().await;
                recorder
                    .record_activity_failed(recorded_at, activity_id, error, attempt)
                    .await
            })
            .map_err(Into::into)
    }

    /// Records activity cancellation through the workflow's single-writer recorder.
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the recorder.
    pub fn record_activity_cancelled(
        &self,
        recorded_at: chrono::DateTime<chrono::Utc>,
        activity_id: ActivityId,
    ) -> Result<(), NifContextError> {
        self.tokio_handle
            .block_on(async {
                let mut recorder = self.recorder.lock().await;
                recorder
                    .record_activity_cancelled(recorded_at, activity_id)
                    .await
            })
            .map_err(Into::into)
    }

    /// Records activity cancellation for a fan-out ordinal and settles its outbox row.
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the recorder.
    pub fn record_activity_cancelled_and_settle_outbox(
        &self,
        recorded_at: chrono::DateTime<chrono::Utc>,
        ordinal: u64,
    ) -> Result<(), NifContextError> {
        self.tokio_handle
            .block_on(async {
                let mut recorder = self.recorder.lock().await;
                recorder
                    .record_activity_cancelled_and_settle_outbox(recorded_at, ordinal)
                    .await
            })
            .map_err(Into::into)
    }

    /// Records a durable fan-out dispatch batch through the workflow's single-writer recorder.
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the recorder.
    pub fn record_fan_out_dispatch(
        &self,
        recorded_at: chrono::DateTime<chrono::Utc>,
        items: &[FanOutItem],
    ) -> Result<(), NifContextError> {
        self.tokio_handle
            .block_on(async {
                let mut recorder = self.recorder.lock().await;
                recorder.record_fan_out_dispatch(recorded_at, items).await
            })
            .map_err(Into::into)
    }

    /// Re-arms the durable outbox rows for a fan-out batch back to claimable `Pending` through the
    /// workflow's single-writer recorder (crash-recovery re-stage).
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the recorder.
    pub fn rearm_outbox_pending(
        &self,
        recorded_at: chrono::DateTime<chrono::Utc>,
        items: &[FanOutItem],
    ) -> Result<(), NifContextError> {
        self.tokio_handle
            .block_on(async {
                let recorder = self.recorder.lock().await;
                recorder.rearm_outbox_pending(recorded_at, items).await
            })
            .map_err(Into::into)
    }

    /// Records one fan-out completion through the workflow's single-writer recorder.
    ///
    /// # Errors
    ///
    /// Propagates any [`DurabilityError`] returned by the recorder.
    pub fn record_fan_out_completion(
        &self,
        recorded_at: chrono::DateTime<chrono::Utc>,
        ordinal: u64,
        outcome: FanOutOutcome,
    ) -> Result<FanOutCompletionResult, NifContextError> {
        self.tokio_handle
            .block_on(async {
                let mut recorder = self.recorder.lock().await;
                recorder
                    .record_fan_out_completion(recorded_at, ordinal, outcome)
                    .await
            })
            .map_err(Into::into)
    }

    /// Returns a snapshot of the recorded history visible to this NIF context.
    #[must_use]
    pub fn history(&self) -> &[aion_core::Event] {
        self.resolver.history()
    }

    /// Resolves a workflow command against recorded history before any live side effect runs.
    ///
    /// # Errors
    ///
    /// Returns [`NifContextError::Durability`] when replay detects non-determinism or malformed
    /// command history.
    pub fn resolve_command(&mut self, command: Command) -> Result<ResolveOutcome, NifContextError> {
        // This resolver was built fresh for one NIF call, with its cursor at
        // the top of history; commands consumed by earlier calls in the same
        // live execution sit before the one being resolved. Skip to this
        // command's correlation key so sequential workflow steps never
        // re-read earlier recorded results. AwaitChild has no positional
        // key — its replay identity is the awaited child workflow id — so it
        // skips to that child's recorded terminal outcome instead.
        if let Some(key) = command.key() {
            self.resolver.fast_forward_to(key);
        } else if let Command::AwaitChild { child_workflow_id } = &command {
            self.resolver
                .fast_forward_to_child_terminal(child_workflow_id);
        }
        self.resolver.resolve(command).map_err(Into::into)
    }
}

fn registry_error_to_context(error: &EngineError) -> NifContextError {
    match error {
        EngineError::RegistryPoisoned => NifContextError::RecorderPoisoned,
        _ => NifContextError::TermEncoding {
            reason: format!("registry lookup failed: {error}"),
        },
    }
}

/// Resolve the workflow handle for `pid`, waiting out the registration birth
/// window.
///
/// The start path spawns the workflow process and only then inserts its
/// handle into the registry, so a workflow whose first instructions call a
/// NIF can legitimately execute before its handle exists. Failing typed in
/// that window kills the workflow at startup: the SDK and fixtures treat a
/// context failure from `receive_signal`/`sleep`/`register_query` as fatal
/// (`{badmatch, {error, ...}}`). The wait is bounded by the engine's
/// builder-supplied delivery policy and converges as soon as the start
/// thread's insert lands. The budget is the policy's full persistence —
/// `ready_timeout × max_enqueue_attempts`, the same product the enqueue
/// retry path expresses — not a single `ready_timeout`: the caller is a
/// live process already executing on this engine's scheduler, so a missing
/// entry is virtually always the in-flight insert, and the cost of giving
/// up early is a workflow killed at birth (`ready_timeout` alone lost to
/// OS-level preemption of the start thread roughly once per few thousand
/// births under heavy host oversubscription). A pid that never appears
/// (a non-workflow process misusing a workflow NIF, or a start rolled back
/// with the pid cancelled) still fails typed after the budget.
fn resolve_handle_with_birth_wait(
    registry: &Registry,
    pid: u64,
    birth_wait: crate::runtime::SignalDeliveryConfig,
) -> Result<WorkflowHandle, NifContextError> {
    let lookup = |registry: &Registry| -> Result<Option<WorkflowHandle>, NifContextError> {
        Ok(registry
            .list()
            .map_err(|error| registry_error_to_context(&error))?
            .into_iter()
            .find(|handle| handle.pid() == pid))
    };
    if let Some(handle) = lookup(registry)? {
        return Ok(handle);
    }
    let budget = birth_wait
        .ready_timeout
        .saturating_mul(birth_wait.max_enqueue_attempts.max(1));
    let deadline = std::time::Instant::now() + budget;
    let mut backoff = birth_wait.initial_backoff;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(backoff);
        let doubled = backoff.saturating_mul(2);
        backoff = if doubled > birth_wait.max_backoff {
            birth_wait.max_backoff
        } else {
            doubled
        };
        if let Some(handle) = lookup(registry)? {
            return Ok(handle);
        }
    }
    Err(NifContextError::UnknownProcess { pid })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{ActivityId, Event, EventEnvelope, Payload, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore, WriteToken};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::{NifContext, NifContextError};
    use crate::durability::{Command, CorrelationKey, Recorder, Resolution, ResolveOutcome};
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn hash() -> ContentHash {
        ContentHash::from_bytes([7; 32])
    }

    /// Fast birth-wait policy for tests: small budget, tight polls.
    fn birth_wait() -> crate::runtime::SignalDeliveryConfig {
        crate::runtime::SignalDeliveryConfig::new(
            std::time::Duration::from_millis(200),
            1,
            std::time::Duration::from_millis(2),
            std::time::Duration::from_millis(8),
        )
    }

    fn payload(label: &str) -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(&json!({ "label": label }))?)
    }

    fn envelope(
        workflow_id: &aion_core::WorkflowId,
        seq: u64,
    ) -> Result<EventEnvelope, Box<dyn std::error::Error>> {
        let recorded_at = Utc
            .timestamp_opt(i64::try_from(seq)?, 0)
            .single()
            .ok_or_else(|| "invalid timestamp".to_owned())?;
        Ok(EventEnvelope {
            seq,
            recorded_at,
            workflow_id: workflow_id.clone(),
        })
    }

    fn started_event(
        workflow_id: &aion_core::WorkflowId,
        run_id: &aion_core::RunId,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(workflow_id, 1)?,
            workflow_type: "checkout".to_owned(),
            input: payload("input")?,
            run_id: run_id.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn handle(
        pid: u64,
        store: Arc<dyn EventStore>,
        workflow_id: aion_core::WorkflowId,
        run_id: aion_core::RunId,
    ) -> WorkflowHandle {
        let recorder = Recorder::resume_at(workflow_id.clone(), store, 1);
        WorkflowHandle::new(WorkflowHandleParts {
            workflow_id,
            run_id,
            pid,
            workflow_type: "checkout".to_owned(),
            namespace: String::from("default"),
            loaded_version: hash(),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        })
    }

    type TestContext = (Registry, Arc<dyn EventStore>, WorkflowHandle);

    fn context_with_history(
        runtime: &tokio::runtime::Runtime,
        pid: u64,
        workflow_id: aion_core::WorkflowId,
        history: &[Event],
    ) -> Result<TestContext, Box<dyn std::error::Error>> {
        let registry = Registry::default();
        let run_id = aion_core::RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let mut full_history = vec![started_event(&workflow_id, &run_id)?];
        full_history.extend_from_slice(history);
        runtime.block_on(store.append(WriteToken::recorder(), &workflow_id, &full_history, 0))?;
        let recorder = Recorder::resume_at(
            workflow_id.clone(),
            Arc::clone(&store),
            full_history.len() as u64,
        );
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "checkout".to_owned(),
            namespace: String::from("default"),
            loaded_version: hash(),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle.clone())?;
        Ok((registry, store, handle))
    }

    #[test]
    fn resolves_registered_pid_to_context() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        runtime.block_on(store.append(
            WriteToken::recorder(),
            &workflow_id,
            &[started_event(&workflow_id, &run_id)?],
            0,
        ))?;
        let handle = handle(44, Arc::clone(&store), workflow_id.clone(), run_id.clone());
        registry.insert((workflow_id.clone(), run_id), handle)?;

        let context = NifContext::new(44, &registry, runtime.handle().clone(), birth_wait())?;

        assert_eq!(context.workflow_id(), &workflow_id);
        assert_eq!(context.pid(), 44);
        Ok(())
    }

    #[test]
    fn unknown_pid_returns_unknown_process() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let registry = Registry::default();

        let error = NifContext::new(77, &registry, runtime.handle().clone(), birth_wait())
            .err()
            .ok_or("expected unknown process error")?;

        assert!(matches!(error, NifContextError::UnknownProcess { pid: 77 }));
        Ok(())
    }

    /// F8 registration race: the start path spawns the workflow process and
    /// only then inserts its registry handle, so a workflow's first NIF call
    /// can run before the handle exists. Context resolution must wait out
    /// that birth window instead of failing typed — the SDK and fixtures
    /// treat a context failure as fatal, so before the fix the workflow died
    /// at startup with `{badmatch, {error, <<"unknown_process:N">>}}` (this
    /// test then failed with `UnknownProcess`).
    #[test]
    fn birth_window_registration_resolves_instead_of_failing() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let registry = Arc::new(Registry::default());
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        runtime.block_on(store.append(
            WriteToken::recorder(),
            &workflow_id,
            &[started_event(&workflow_id, &run_id)?],
            0,
        ))?;
        let handle = handle(91, Arc::clone(&store), workflow_id.clone(), run_id.clone());

        // The "start thread": inserts the registry handle a beat after the
        // workflow's first NIF call began resolving its context.
        let late_registry = Arc::clone(&registry);
        let inserter = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            late_registry.insert((workflow_id.clone(), run_id), handle)
        });

        let context = NifContext::new(91, &registry, runtime.handle().clone(), birth_wait())?;

        assert_eq!(context.pid(), 91);
        inserter
            .join()
            .map_err(|_| "registry insert thread panicked")??;
        Ok(())
    }

    #[test]
    fn block_on_recorder_reads_current_head_without_deadlock() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        runtime.block_on(store.append(
            WriteToken::recorder(),
            &workflow_id,
            &[started_event(&workflow_id, &run_id)?],
            0,
        ))?;
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), 5);
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid: 55,
            workflow_type: "checkout".to_owned(),
            namespace: String::from("default"),
            loaded_version: hash(),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle)?;
        let context = NifContext::new(55, &registry, runtime.handle().clone(), birth_wait())?;

        let head = context
            .block_on_recorder(|recorder| Box::pin(async move { Ok(recorder.current_head()) }))?;

        assert_eq!(head, 5);
        Ok(())
    }

    #[test]
    fn resolve_command_returns_recorded_activity_resolution() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let result = payload("activity-result")?;
        let history = vec![
            Event::ActivityScheduled {
                envelope: envelope(&workflow_id, 2)?,
                activity_id: ActivityId::from_sequence_position(0),
                activity_type: "activity".to_owned(),
                input: payload("activity-input")?,
            },
            Event::ActivityCompleted {
                envelope: envelope(&workflow_id, 3)?,
                activity_id: ActivityId::from_sequence_position(0),
                result: result.clone(),
            },
        ];
        let (registry, store, handle) = context_with_history(&runtime, 66, workflow_id, &history)?;
        let mut context = NifContext::new_with_history_store(
            66,
            &registry,
            runtime.handle().clone(),
            Some(store),
            birth_wait(),
        )?;

        assert_eq!(context.workflow_id(), handle.workflow_id());
        assert_eq!(
            context.resolve_command(Command::RunActivity {
                key: CorrelationKey::Activity(0),
                activity_type: "activity".to_owned(),
                input: payload("activity-input")?,
            })?,
            ResolveOutcome::Recorded(Resolution::ActivityCompleted(result))
        );
        Ok(())
    }

    fn child_history(
        workflow_id: &aion_core::WorkflowId,
        child_workflow_id: &aion_core::WorkflowId,
        include_terminal: bool,
    ) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let timer_id = aion_core::TimerId::anonymous(0);
        let mut history = vec![
            Event::ActivityScheduled {
                envelope: envelope(workflow_id, 2)?,
                activity_id: ActivityId::from_sequence_position(0),
                activity_type: "activity".to_owned(),
                input: payload("activity-input")?,
            },
            Event::ActivityCompleted {
                envelope: envelope(workflow_id, 3)?,
                activity_id: ActivityId::from_sequence_position(0),
                result: payload("activity-result")?,
            },
            Event::TimerStarted {
                envelope: envelope(workflow_id, 4)?,
                timer_id: timer_id.clone(),
                fire_at: Utc
                    .timestamp_opt(99, 0)
                    .single()
                    .ok_or_else(|| "invalid timestamp".to_owned())?,
            },
            Event::TimerFired {
                envelope: envelope(workflow_id, 5)?,
                timer_id,
            },
            Event::ChildWorkflowStarted {
                envelope: envelope(workflow_id, 6)?,
                child_workflow_id: child_workflow_id.clone(),
                workflow_type: "child".to_owned(),
                input: payload("child-input")?,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
        ];
        if include_terminal {
            history.push(Event::ChildWorkflowCompleted {
                envelope: envelope(workflow_id, 7)?,
                child_workflow_id: child_workflow_id.clone(),
                result: payload("child-result")?,
            });
        }
        Ok(history)
    }

    #[test]
    fn await_child_skips_consumed_commands_to_recorded_terminal() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let child_workflow_id = aion_core::WorkflowId::new_v4();
        // Activity, timer, and spawn history all precede the awaited child's
        // terminal: each per-NIF resolver starts at the top of history, so
        // AwaitChild must skip those consumed commands instead of reporting
        // a false non-determinism mismatch on the first matchable event.
        let history = child_history(&workflow_id, &child_workflow_id, true)?;
        let (registry, store, _handle) = context_with_history(&runtime, 88, workflow_id, &history)?;
        let mut context = NifContext::new_with_history_store(
            88,
            &registry,
            runtime.handle().clone(),
            Some(store),
            birth_wait(),
        )?;

        assert_eq!(
            context.resolve_command(Command::AwaitChild {
                child_workflow_id: child_workflow_id.clone(),
            })?,
            ResolveOutcome::Recorded(Resolution::ChildCompleted(payload("child-result")?))
        );
        Ok(())
    }

    #[test]
    fn await_child_without_recorded_terminal_resumes_live() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let child_workflow_id = aion_core::WorkflowId::new_v4();
        // History ends after ChildWorkflowStarted (crash mid-child): the
        // await must hand off to live execution for the same child instead
        // of mismatching on the recorded start event.
        let history = child_history(&workflow_id, &child_workflow_id, false)?;
        let (registry, store, _handle) = context_with_history(&runtime, 89, workflow_id, &history)?;
        let mut context = NifContext::new_with_history_store(
            89,
            &registry,
            runtime.handle().clone(),
            Some(store),
            birth_wait(),
        )?;

        assert_eq!(
            context.resolve_command(Command::AwaitChild { child_workflow_id })?,
            ResolveOutcome::ResumeLive
        );
        Ok(())
    }
}
