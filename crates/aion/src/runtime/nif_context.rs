//! Per-call NIF context resolution and durability replay checks.

use std::future::Future;
use std::sync::Arc;

use aion_core::{RunId, WorkflowId};
use aion_store::EventStore;
use beamr::atom::Atom;
use beamr::term::Term;
use beamr::term::binary;
use beamr::term::boxed;
use tokio::runtime::Handle;
use tokio::sync::Mutex;

use crate::EngineError;
use crate::durability::{
    Command, DurabilityError, HistoryCursor, Recorder, ResolveOutcome, Resolver,
};
use crate::registry::{Registry, WorkflowHandle};

thread_local! {
    static ERROR_HEAP: std::cell::RefCell<Vec<Box<[u64]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn park_heap(heap: Box<[u64]>) {
    ERROR_HEAP.with_borrow_mut(|parked| parked.push(heap));
}

fn alloc_binary_term(bytes: &[u8]) -> Option<Term> {
    let word_count = 2 + binary::packed_word_count(bytes.len());
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = binary::write_binary(&mut heap, bytes)?;
    park_heap(heap);
    Some(term)
}

fn alloc_tuple_term(elements: &[Term]) -> Option<Term> {
    let word_count = 1 + elements.len();
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = boxed::write_tuple(&mut heap, elements)?;
    park_heap(heap);
    Some(term)
}

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
    /// Converts this error into the NIF convention `{error, <<reason>>}` term.
    ///
    /// # Errors
    ///
    /// Returns [`NifContextError::TermEncoding`] when the BEAM term heap allocation helpers fail.
    pub fn to_error_term(&self) -> Result<Term, Self> {
        let message = self.error_reason();
        let value = alloc_binary_term(message.as_bytes()).ok_or_else(|| Self::TermEncoding {
            reason: "failed to allocate error binary term".to_owned(),
        })?;
        alloc_tuple_term(&[Term::atom(Atom::ERROR), value]).ok_or_else(|| Self::TermEncoding {
            reason: "failed to allocate error tuple term".to_owned(),
        })
    }

    fn error_reason(&self) -> String {
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
}

impl NifContext {
    /// Resolves `pid` against the active registry and builds a replay resolver from recorded history.
    ///
    /// # Errors
    ///
    /// Returns [`NifContextError::UnknownProcess`] when the registry has no matching active handle,
    /// or [`NifContextError::Durability`] when recorded history cannot be read or cursor-validated.
    pub fn new(
        pid: u64,
        registry: &Registry,
        tokio_handle: Handle,
    ) -> Result<Self, NifContextError> {
        Self::new_with_history_store(pid, registry, tokio_handle, None)
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
    ) -> Result<Self, NifContextError> {
        let handle = registry
            .list()
            .map_err(registry_error_to_context)?
            .into_iter()
            .find(|handle| handle.pid() == pid)
            .ok_or(NifContextError::UnknownProcess { pid })?;
        let recorder = handle.recorder();
        let workflow_id = handle.workflow_id().clone();
        let history = match store {
            Some(store) => tokio_handle
                .block_on(store.read_history(&workflow_id))
                .map_err(|error| NifContextError::Durability(error.into()))?,
            None => tokio_handle.block_on(async {
                let recorder = recorder.lock().await;
                recorder.read_history().await
            })?,
        };
        let cursor = HistoryCursor::new(history)?;
        let resolver = Resolver::new(workflow_id, cursor);

        Ok(Self {
            handle,
            recorder,
            tokio_handle,
            resolver,
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

    /// Returns the runtime process identifier for the resolved handle.
    #[must_use]
    pub const fn pid(&self) -> u64 {
        self.handle.pid()
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

    /// Resolves a workflow command against recorded history before any live side effect runs.
    ///
    /// # Errors
    ///
    /// Returns [`NifContextError::Durability`] when replay detects non-determinism or malformed
    /// command history.
    pub fn resolve_command(&mut self, command: Command) -> Result<ResolveOutcome, NifContextError> {
        self.resolver.resolve(command).map_err(Into::into)
    }
}

fn registry_error_to_context(error: EngineError) -> NifContextError {
    match error {
        EngineError::RegistryPoisoned => NifContextError::RecorderPoisoned,
        _ => NifContextError::TermEncoding {
            reason: format!("registry lookup failed: {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{ActivityId, Event, EventEnvelope, Payload, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore};
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

    fn handle(
        pid: u64,
        store: Arc<dyn EventStore>,
        workflow_id: aion_core::WorkflowId,
    ) -> WorkflowHandle {
        let recorder = Recorder::new(workflow_id.clone(), store);
        WorkflowHandle::new(WorkflowHandleParts {
            workflow_id,
            run_id: aion_core::RunId::new_v4(),
            pid,
            workflow_type: "checkout".to_owned(),
            loaded_version: hash(),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        })
    }

    fn context_with_history(
        runtime: &tokio::runtime::Runtime,
        pid: u64,
        workflow_id: aion_core::WorkflowId,
        history: &[Event],
    ) -> Result<(Registry, Arc<dyn EventStore>, WorkflowHandle), Box<dyn std::error::Error>> {
        let registry = Registry::default();
        let run_id = aion_core::RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        if !history.is_empty() {
            runtime.block_on(store.append(&workflow_id, history, 0))?;
        }
        let recorder = Recorder::resume_at(
            workflow_id.clone(),
            Arc::clone(&store),
            history.len() as u64,
        );
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "checkout".to_owned(),
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
        let handle = handle(44, Arc::clone(&store), workflow_id.clone());
        registry.insert((workflow_id.clone(), run_id), handle)?;

        let context = NifContext::new(44, &registry, runtime.handle().clone())?;

        assert_eq!(context.workflow_id(), &workflow_id);
        assert_eq!(context.pid(), 44);
        Ok(())
    }

    #[test]
    fn unknown_pid_returns_unknown_process() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let registry = Registry::default();

        let error = NifContext::new(77, &registry, runtime.handle().clone())
            .err()
            .ok_or("expected unknown process error")?;

        assert!(matches!(error, NifContextError::UnknownProcess { pid: 77 }));
        Ok(())
    }

    #[test]
    fn block_on_recorder_reads_current_head_without_deadlock() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), 5);
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid: 55,
            workflow_type: "checkout".to_owned(),
            loaded_version: hash(),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle)?;
        let context = NifContext::new(55, &registry, runtime.handle().clone())?;

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
                envelope: envelope(&workflow_id, 1)?,
                activity_id: ActivityId::from_sequence_position(0),
                activity_type: "activity".to_owned(),
                input: payload("activity-input")?,
            },
            Event::ActivityCompleted {
                envelope: envelope(&workflow_id, 2)?,
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
}
