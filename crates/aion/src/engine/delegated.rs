//! signal/query/subscribe surface (AT/AD delegation)

use aion_core::{Event, Payload, RunId, WorkflowId, current_lease_terminal, run_segment};
use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{Engine, EngineError, SignalRouterError, WorkflowHandle};

use super::api::workflow_not_found;

/// Live-event subscription filter consumed by the AD/AT publisher seam.
///
/// The `run` field is part of the cross-cluster contract even though the
/// current core [`Event`] envelope does not yet carry run metadata; publisher
/// implementations that know run residency out-of-band can apply it there.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct EventFilter {
    /// Match events for this workflow execution id.
    pub workflow_id: Option<WorkflowId>,
    /// Match events for this run id when the publisher has run metadata.
    pub run: Option<RunId>,
    /// Match events belonging to this event family.
    pub family: Option<EventFamily>,
}

impl EventFilter {
    /// Returns whether an event satisfies the constraints visible on [`Event`].
    ///
    /// Run filtering is intentionally not decided here because [`Event`] does
    /// not currently include a [`RunId`]; the publisher seam applies that field
    /// with its own metadata when available.
    #[must_use]
    pub fn matches(&self, event: &Event) -> bool {
        self.workflow_id
            .as_ref()
            .is_none_or(|workflow_id| event.workflow_id() == workflow_id)
            && self
                .family
                .is_none_or(|family| family == event_family(event))
    }
}

/// Coarse event families for live subscription filtering.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum EventFamily {
    /// Workflow lifecycle events.
    Workflow,
    /// Activity scheduling and completion events.
    Activity,
    /// Timer events owned by AT.
    Timer,
    /// Signal delivery events owned by AT/AD.
    Signal,
    /// Child-workflow lifecycle events.
    ChildWorkflow,
    /// Schedule lifecycle and trigger events.
    Schedule,
}

/// AT-005/AT-006 signal-routing seam.
///
/// Implementations record the signal through the workflow recorder and deliver
/// it to the target workflow mailbox. The engine only resolves the live target
/// handle and delegates to this trait.
#[async_trait]
pub trait SignalRouter: Send + Sync {
    /// Route a signal to the already-resolved workflow target.
    async fn route(
        &self,
        target: &WorkflowHandle,
        name: String,
        payload: Payload,
    ) -> Result<(), EngineError>;
}

/// AT-007 query-dispatch seam.
///
/// Implementations dispatch a read-only query to workflow code and map their
/// query errors into [`EngineError`]. The engine only resolves the live target
/// handle and delegates to this trait.
#[async_trait]
pub trait QueryService: Send + Sync {
    /// Dispatch a named query to the already-resolved workflow target.
    async fn query(&self, target: &WorkflowHandle, name: String) -> Result<Payload, EngineError>;
}

/// A live subscription fell behind the publisher and skipped events.
///
/// Publishers yield this as a stream item — never a silent skip and never a
/// silent stream end — and then continue with subsequent live events, so the
/// consumer always learns exactly how many events it missed.
#[derive(thiserror::Error, Clone, Copy, Debug, PartialEq, Eq)]
#[error("event subscription lagged behind the live stream and skipped {skipped} events")]
pub struct EventStreamLagged {
    /// Number of events the subscriber missed.
    pub skipped: u64,
}

/// AD/AT live event publisher seam.
///
/// Implementations own event publication and filtering. The engine exposes the
/// in-process subscription surface without owning publication machinery.
pub trait EventPublisher: Send + Sync {
    /// Subscribe to a filtered stream of live workflow events.
    ///
    /// A subscriber that falls behind the publisher receives one
    /// `Err(`[`EventStreamLagged`]`)` item carrying the skipped count and then
    /// continues with subsequent events; lag never silently drops events and
    /// never silently ends the stream.
    fn subscribe(
        &self,
        filter: EventFilter,
    ) -> BoxStream<'static, Result<Event, EventStreamLagged>>;
}

/// Object-safe delegated seams held by the engine for AT/AD integration.
#[derive(Clone)]
pub struct DelegatedSeams {
    signal_router: Arc<dyn SignalRouter>,
    query_service: Arc<dyn QueryService>,
    event_publisher: Arc<dyn EventPublisher>,
}

impl DelegatedSeams {
    /// Build a seam bundle from concrete AT/AD implementations.
    #[must_use]
    pub const fn new(
        signal_router: Arc<dyn SignalRouter>,
        query_service: Arc<dyn QueryService>,
        event_publisher: Arc<dyn EventPublisher>,
    ) -> Self {
        Self {
            signal_router,
            query_service,
            event_publisher,
        }
    }

    /// Signal routing seam installed for AT-005/AT-006 delegation.
    #[must_use]
    pub fn signal_router(&self) -> &dyn SignalRouter {
        self.signal_router.as_ref()
    }

    /// Query dispatch seam installed for AT-007 delegation.
    #[must_use]
    pub fn query_service(&self) -> &dyn QueryService {
        self.query_service.as_ref()
    }

    /// Live event publisher seam installed for AD/AT delegation.
    #[must_use]
    pub fn event_publisher(&self) -> &dyn EventPublisher {
        self.event_publisher.as_ref()
    }

    pub(crate) fn signal_router_arc(&self) -> Arc<dyn SignalRouter> {
        Arc::clone(&self.signal_router)
    }

    pub(crate) fn query_service_arc(&self) -> Arc<dyn QueryService> {
        Arc::clone(&self.query_service)
    }

    pub(crate) fn event_publisher_arc(&self) -> Arc<dyn EventPublisher> {
        Arc::clone(&self.event_publisher)
    }
}

impl Default for DelegatedSeams {
    fn default() -> Self {
        Self::new(
            Arc::new(DeferredSignalRouter),
            Arc::new(DeferredQueryService),
            Arc::new(DeferredEventPublisher),
        )
    }
}

/// Deferred signal seam used until AT-005/AT-006 installs a concrete router.
#[derive(Debug, Default)]
pub struct DeferredSignalRouter;

#[async_trait]
impl SignalRouter for DeferredSignalRouter {
    async fn route(
        &self,
        target: &WorkflowHandle,
        name: String,
        payload: Payload,
    ) -> Result<(), EngineError> {
        let _ = (target, name, payload);
        Err(EngineError::Runtime {
            reason: "signal routing seam is not configured".to_owned(),
        })
    }
}

/// Deferred query seam used until AT-007 installs a concrete service.
#[derive(Debug, Default)]
pub struct DeferredQueryService;

#[async_trait]
impl QueryService for DeferredQueryService {
    async fn query(&self, target: &WorkflowHandle, name: String) -> Result<Payload, EngineError> {
        let _ = (target, name);
        Err(EngineError::Runtime {
            reason: "query service seam is not configured".to_owned(),
        })
    }
}

/// Deferred publisher seam used until AD/AT installs a concrete publisher.
#[derive(Debug, Default)]
pub struct DeferredEventPublisher;

impl EventPublisher for DeferredEventPublisher {
    fn subscribe(
        &self,
        filter: EventFilter,
    ) -> BoxStream<'static, Result<Event, EventStreamLagged>> {
        let _ = filter;
        Box::pin(stream::empty())
    }
}

impl Engine {
    /// Send a signal to a live workflow run through the AT routing seam.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair is unknown,
    /// [`SignalRouterError::Terminal`] when it is durably terminal, or other typed errors from
    /// the configured signal seam.
    pub async fn signal(
        &self,
        id: &WorkflowId,
        run: &RunId,
        name: impl Into<String>,
        payload: Payload,
    ) -> Result<(), EngineError> {
        let handle = if let Some(handle) = self.registry().get(id, run)? {
            handle
        } else {
            let history = self.store().read_history(id).await?;
            if run_has_terminal_history(&history, run) {
                return Err(SignalRouterError::Terminal {
                    workflow_id: id.clone(),
                    run_id: run.clone(),
                }
                .into());
            }
            self.handle_after_birth_window(id, run, &history)
                .await?
                .ok_or_else(|| workflow_not_found(id, run))?
        };
        self.delegated()
            .signal_router()
            .route(&handle, name.into(), payload)
            .await
    }

    /// Dispatch a read-only query to a live workflow run through the AT seam.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Query`] with [`crate::query::QueryError::NotRunning`]
    /// when the `(workflow, run)` pair is durably terminal,
    /// [`EngineError::WorkflowNotFound`] when it is unknown, and other typed
    /// errors from the configured query seam.
    pub async fn query(
        &self,
        id: &WorkflowId,
        run: &RunId,
        name: impl Into<String>,
    ) -> Result<Payload, EngineError> {
        let handle = if let Some(handle) = self.registry().get(id, run)? {
            handle
        } else {
            // Mirror Engine::signal's registry-miss handling: a completed
            // workflow is NotRunning per the query contract, never NotFound.
            let history = self.store().read_history(id).await?;
            if run_has_terminal_history(&history, run) {
                return Err(EngineError::Query(crate::query::QueryError::NotRunning(
                    id.clone(),
                )));
            }
            self.handle_after_birth_window(id, run, &history)
                .await?
                .ok_or_else(|| workflow_not_found(id, run))?
        };
        self.delegated()
            .query_service()
            .query(&handle, name.into())
            .await
    }

    /// Resolve a registry miss against the registration birth window.
    ///
    /// The start path records `WorkflowStarted` durably *before* it inserts
    /// the registry handle, so an embedder acting on observed history (or on
    /// a `start_workflow` racing on another task) can legitimately arrive
    /// here after the record and before the insert. When the requested run
    /// is durably started and non-terminal, the registry is re-polled within
    /// the builder-supplied delivery policy budget; `None` after the budget
    /// means the run truly has no live handle (its start failed or its
    /// engine is gone) and the caller fails typed.
    pub(crate) async fn handle_after_birth_window(
        &self,
        id: &WorkflowId,
        run: &RunId,
        history: &[Event],
    ) -> Result<Option<WorkflowHandle>, EngineError> {
        let started = history
            .iter()
            .any(|event| matches!(event, Event::WorkflowStarted { run_id, .. } if run_id == run));
        if !started {
            return Ok(None);
        }
        wait_for_registered_handle(self.registry(), id, run, self.runtime().signal_delivery()).await
    }

    /// Subscribe to the live event stream through the AD/AT publisher seam.
    ///
    /// A subscriber that falls behind receives one `Err(`[`EventStreamLagged`]`)`
    /// item with the skipped count and then continues with subsequent events.
    #[must_use]
    pub fn subscribe(
        &self,
        filter: EventFilter,
    ) -> BoxStream<'static, Result<Event, EventStreamLagged>> {
        self.delegated().event_publisher().subscribe(filter)
    }
}

pub(crate) fn run_has_terminal_history(history: &[Event], run: &RunId) -> bool {
    // Reset-aware: a run is terminal only if its current lease ended in a
    // terminal event. A WorkflowReopened after a terminal reopens the run, so a
    // reopened run is not treated as terminal (it can receive signals and
    // complete again).
    current_lease_terminal(run_segment(history, run)).is_some()
}

/// Poll the registry for `(id, run)` until the handle appears or the
/// builder-supplied delivery budget is spent.
///
/// The budget is the policy's full persistence — `ready_timeout ×
/// max_enqueue_attempts`, the same product the runtime's enqueue retry
/// expresses — polled with the policy's backoff ladder. A single
/// `ready_timeout` is the typical insert latency, but the start thread can
/// be preempted past it under host oversubscription, and the cost of giving
/// up early is a typed not-found for a workflow that is durably started.
pub(crate) async fn wait_for_registered_handle(
    registry: &crate::registry::Registry,
    id: &WorkflowId,
    run: &RunId,
    policy: crate::runtime::SignalDeliveryConfig,
) -> Result<Option<WorkflowHandle>, EngineError> {
    let budget = policy
        .ready_timeout
        .saturating_mul(policy.max_enqueue_attempts.max(1));
    let deadline = std::time::Instant::now() + budget;
    let mut backoff = policy.initial_backoff;
    loop {
        if let Some(handle) = registry.get(id, run)? {
            return Ok(Some(handle));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(backoff).await;
        let doubled = backoff.saturating_mul(2);
        backoff = if doubled > policy.max_backoff {
            policy.max_backoff
        } else {
            doubled
        };
    }
}

const fn event_family(event: &Event) -> EventFamily {
    match event {
        Event::WorkflowStarted { .. }
        | Event::WorkflowCompleted { .. }
        | Event::WorkflowFailed { .. }
        | Event::WorkflowCancelled { .. }
        | Event::WorkflowTimedOut { .. }
        | Event::WorkflowContinuedAsNew { .. }
        | Event::WorkflowReopened { .. }
        | Event::SearchAttributesUpdated { .. } => EventFamily::Workflow,
        Event::ActivityScheduled { .. }
        | Event::ActivityStarted { .. }
        | Event::ActivityCompleted { .. }
        | Event::ActivityFailed { .. }
        | Event::ActivityCancelled { .. } => EventFamily::Activity,
        Event::TimerStarted { .. }
        | Event::TimerFired { .. }
        | Event::TimerCancelled { .. }
        | Event::WithTimeoutCompleted { .. } => EventFamily::Timer,
        Event::SignalReceived { .. } | Event::SignalSent { .. } => EventFamily::Signal,
        Event::ChildWorkflowStarted { .. }
        | Event::ChildWorkflowCompleted { .. }
        | Event::ChildWorkflowFailed { .. }
        | Event::ChildWorkflowCancelled { .. } => EventFamily::ChildWorkflow,
        Event::ScheduleCreated { .. }
        | Event::ScheduleUpdated { .. }
        | Event::SchedulePaused { .. }
        | Event::ScheduleResumed { .. }
        | Event::ScheduleDeleted { .. }
        | Event::ScheduleTriggered { .. } => EventFamily::Schedule,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use aion_core::{EventEnvelope, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore};
    use futures::{StreamExt, stream};
    use serde_json::json;

    use crate::durability::Recorder;
    use crate::engine::api::EngineComponents;
    use crate::registry::{CompletionNotifier, HandleResidency, WorkflowHandleParts};
    use crate::{
        Registry, RuntimeConfig, RuntimeHandle, SupervisionTree, WorkflowCatalog, WorkflowHandle,
    };

    use super::*;

    #[derive(Debug, Default)]
    struct SignalCapture {
        calls: Mutex<Vec<(u64, String, Payload)>>,
    }

    #[async_trait]
    impl SignalRouter for SignalCapture {
        async fn route(
            &self,
            target: &WorkflowHandle,
            name: String,
            payload: Payload,
        ) -> Result<(), EngineError> {
            self.calls
                .lock()
                .map_err(|_| EngineError::RegistryPoisoned)?
                .push((target.pid(), name, payload));
            Ok(())
        }
    }

    #[derive(Debug)]
    struct QueryCapture {
        calls: Mutex<Vec<(u64, String)>>,
        reply: Payload,
    }

    #[async_trait]
    impl QueryService for QueryCapture {
        async fn query(
            &self,
            target: &WorkflowHandle,
            name: String,
        ) -> Result<Payload, EngineError> {
            self.calls
                .lock()
                .map_err(|_| EngineError::RegistryPoisoned)?
                .push((target.pid(), name));
            Ok(self.reply.clone())
        }
    }

    #[derive(Debug)]
    struct FakePublisher {
        events: Vec<Event>,
    }

    impl EventPublisher for FakePublisher {
        fn subscribe(
            &self,
            filter: EventFilter,
        ) -> BoxStream<'static, Result<Event, EventStreamLagged>> {
            let events = self
                .events
                .iter()
                .filter(|event| filter.matches(event))
                .cloned()
                .map(Ok)
                .collect::<Vec<_>>();
            stream::iter(events).boxed()
        }
    }

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn engine_with_seams(
        signal_router: Arc<dyn SignalRouter>,
        query_service: Arc<dyn QueryService>,
        event_publisher: Arc<dyn EventPublisher>,
    ) -> Result<Engine, EngineError> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as _;
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        Ok(Engine::new(EngineComponents {
            store,
            visibility_store,
            runtime: Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?),
            catalog: Arc::new(WorkflowCatalog::new()),
            registry: Arc::new(Registry::default()),
            supervision: Arc::new(SupervisionTree::new()),
            delegated: DelegatedSeams::new(signal_router, query_service, event_publisher),
            signal_handoff: Arc::new(crate::signal::SignalResumeHandoff::new()),
            search_attribute_schema: Arc::new(aion_core::SearchAttributeSchema::new()),
            visibility_reconciliation_task: None,
        }))
    }

    /// Record `WorkflowStarted` durably and build the matching handle
    /// without inserting it into the registry — the exact state of the
    /// registration birth window.
    async fn recorded_active_handle(
        engine: &Engine,
    ) -> Result<WorkflowHandle, Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let store = engine.store();
        let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(&store));
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                crate::durability::WorkflowStartRecord {
                    workflow_type: "checkout".to_owned(),
                    input: payload("input")?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;
        Ok(WorkflowHandle::new(WorkflowHandleParts {
            workflow_id,
            run_id,
            pid: engine.runtime().spawn_test_process_with_trap_exit(true)?,
            workflow_type: "checkout".to_owned(),
            namespace: String::from("default"),
            loaded_version: ContentHash::from_bytes([1; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        }))
    }

    async fn insert_active_handle(
        engine: &Engine,
    ) -> Result<WorkflowHandle, Box<dyn std::error::Error>> {
        let handle = recorded_active_handle(engine).await?;
        engine.registry().insert(
            (handle.workflow_id().clone(), handle.run_id().clone()),
            handle.clone(),
        )?;
        Ok(handle)
    }

    fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        }
    }

    /// Registration birth window (the 1/300 release-signal flake): the start
    /// path records `WorkflowStarted` durably before it inserts the registry
    /// handle, so a caller acting on observed history can signal before the
    /// insert lands. The signal must wait the handle out within the delivery
    /// policy budget — before the fix it returned `WorkflowNotFound`
    /// immediately.
    #[tokio::test(flavor = "multi_thread")]
    async fn signal_inside_the_registration_birth_window_waits_for_the_handle()
    -> Result<(), Box<dyn std::error::Error>> {
        let signal = Arc::new(SignalCapture::default());
        let engine = Arc::new(engine_with_seams(
            signal.clone(),
            Arc::new(DeferredQueryService),
            Arc::new(DeferredEventPublisher),
        )?);
        let handle = recorded_active_handle(&engine).await?;

        // The insert lands mid-wait, exactly as the start thread's does.
        let late_engine = Arc::clone(&engine);
        let late_handle = handle.clone();
        let inserter = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            late_engine.registry().insert(
                (
                    late_handle.workflow_id().clone(),
                    late_handle.run_id().clone(),
                ),
                late_handle,
            )
        });

        engine
            .signal(
                handle.workflow_id(),
                handle.run_id(),
                "approve",
                payload("birth")?,
            )
            .await?;
        inserter.await??;

        let calls = signal
            .calls
            .lock()
            .map_err(|_| EngineError::RegistryPoisoned)?;
        assert_eq!(calls.len(), 1, "the signal must reach the routed handle");
        drop(calls);
        engine.shutdown()?;
        Ok(())
    }

    /// The birth wait is bounded: a durably started run whose handle never
    /// appears (its start failed, or its engine is gone) still fails typed
    /// after the policy budget.
    #[tokio::test(flavor = "multi_thread")]
    async fn signal_for_a_started_run_with_no_handle_fails_typed_after_the_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = engine_with_seams(
            Arc::new(SignalCapture::default()),
            Arc::new(DeferredQueryService),
            Arc::new(DeferredEventPublisher),
        )?;
        let handle = recorded_active_handle(&engine).await?;

        let outcome = engine
            .signal(
                handle.workflow_id(),
                handle.run_id(),
                "approve",
                payload("never")?,
            )
            .await;

        assert!(matches!(outcome, Err(EngineError::WorkflowNotFound { .. })));
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn signal_delegates_to_router_and_unknown_returns_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let signal = Arc::new(SignalCapture::default());
        let engine = engine_with_seams(
            signal.clone(),
            Arc::new(DeferredQueryService),
            Arc::new(DeferredEventPublisher),
        )?;
        let handle = insert_active_handle(&engine).await?;
        let sent_payload = payload("signal")?;

        engine
            .signal(
                handle.workflow_id(),
                handle.run_id(),
                "approve",
                sent_payload.clone(),
            )
            .await?;

        {
            let calls = signal
                .calls
                .lock()
                .map_err(|_| EngineError::RegistryPoisoned)?;
            assert_eq!(
                calls.as_slice(),
                &[(handle.pid(), "approve".to_owned(), sent_payload)]
            );
        }
        let unknown = engine
            .signal(
                &WorkflowId::new_v4(),
                &RunId::new_v4(),
                "approve",
                payload("unknown")?,
            )
            .await;
        assert!(matches!(unknown, Err(EngineError::WorkflowNotFound { .. })));
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn query_delegates_to_service_and_returns_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let reply = payload("reply")?;
        let query = Arc::new(QueryCapture {
            calls: Mutex::new(Vec::new()),
            reply: reply.clone(),
        });
        let engine = engine_with_seams(
            Arc::new(DeferredSignalRouter),
            query.clone(),
            Arc::new(DeferredEventPublisher),
        )?;
        let handle = insert_active_handle(&engine).await?;

        let returned = engine
            .query(handle.workflow_id(), handle.run_id(), "state")
            .await?;

        assert_eq!(returned, reply);
        let calls = query
            .calls
            .lock()
            .map_err(|_| EngineError::RegistryPoisoned)?;
        assert_eq!(calls.as_slice(), &[(handle.pid(), "state".to_owned())]);
        drop(calls);
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn query_terminal_run_is_not_running_and_unknown_is_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = engine_with_seams(
            Arc::new(DeferredSignalRouter),
            Arc::new(DeferredQueryService),
            Arc::new(DeferredEventPublisher),
        )?;
        // Durably terminal run with no registry entry: a completed workflow.
        let workflow_id = WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = crate::durability::Recorder::new(workflow_id.clone(), engine.store());
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                crate::durability::WorkflowStartRecord {
                    workflow_type: "checkout".to_owned(),
                    input: payload("input")?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;
        recorder
            .record_workflow_completed(chrono::Utc::now(), payload("result")?)
            .await?;

        let terminal = engine.query(&workflow_id, &run_id, "state").await;
        assert!(matches!(
            terminal,
            Err(EngineError::Query(crate::query::QueryError::NotRunning(id))) if id == workflow_id
        ));

        let unknown = engine
            .query(&WorkflowId::new_v4(), &RunId::new_v4(), "state")
            .await;
        assert!(matches!(unknown, Err(EngineError::WorkflowNotFound { .. })));
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn subscribe_delegates_to_publisher_stream_with_filter()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let other_id = WorkflowId::new_v4();
        let matching = Event::SignalReceived {
            envelope: envelope(1, &workflow_id),
            name: "approved".to_owned(),
            payload: payload("signal")?,
        };
        let filtered = Event::WorkflowStarted {
            envelope: envelope(1, &other_id),
            workflow_type: "checkout".to_owned(),
            input: payload("input")?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        };
        let engine = engine_with_seams(
            Arc::new(DeferredSignalRouter),
            Arc::new(DeferredQueryService),
            Arc::new(FakePublisher {
                events: vec![matching.clone(), filtered],
            }),
        )?;

        let events = engine
            .subscribe(EventFilter {
                workflow_id: Some(workflow_id),
                run: None,
                family: Some(EventFamily::Signal),
            })
            .collect::<Vec<_>>()
            .await;

        assert_eq!(events, vec![Ok(matching)]);
        engine.shutdown()?;
        Ok(())
    }
}
