//! signal/query/subscribe surface (AT/AD delegation)

use aion_core::{Event, Payload, RunId, WorkflowId};
use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{Engine, EngineError, WorkflowHandle};

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

/// AD/AT live event publisher seam.
///
/// Implementations own event publication and filtering. The engine exposes the
/// in-process subscription surface without owning publication machinery.
pub trait EventPublisher: Send + Sync {
    /// Subscribe to a filtered stream of live workflow events.
    fn subscribe(&self, filter: EventFilter) -> BoxStream<'static, Event>;
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
    fn subscribe(&self, filter: EventFilter) -> BoxStream<'static, Event> {
        let _ = filter;
        Box::pin(stream::empty())
    }
}

impl Engine {
    /// Send a signal to a live workflow run through the AT routing seam.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair
    /// is not live. Other typed errors come from the configured signal seam.
    pub async fn signal(
        &self,
        id: &WorkflowId,
        run: &RunId,
        name: impl Into<String>,
        payload: Payload,
    ) -> Result<(), EngineError> {
        let handle = self
            .registry()
            .get(id, run)?
            .ok_or_else(|| workflow_not_found(id, run))?;
        self.delegated()
            .signal_router()
            .route(&handle, name.into(), payload)
            .await
    }

    /// Dispatch a read-only query to a live workflow run through the AT seam.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair
    /// is not live. Other typed errors come from the configured query seam.
    pub async fn query(
        &self,
        id: &WorkflowId,
        run: &RunId,
        name: impl Into<String>,
    ) -> Result<Payload, EngineError> {
        let handle = self
            .registry()
            .get(id, run)?
            .ok_or_else(|| workflow_not_found(id, run))?;
        self.delegated()
            .query_service()
            .query(&handle, name.into())
            .await
    }

    /// Subscribe to the live event stream through the AD/AT publisher seam.
    #[must_use]
    pub fn subscribe(&self, filter: EventFilter) -> BoxStream<'static, Event> {
        self.delegated().event_publisher().subscribe(filter)
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
        | Event::SearchAttributesUpdated { .. } => EventFamily::Workflow,
        Event::ActivityScheduled { .. }
        | Event::ActivityStarted { .. }
        | Event::ActivityCompleted { .. }
        | Event::ActivityFailed { .. }
        | Event::ActivityCancelled { .. } => EventFamily::Activity,
        Event::TimerStarted { .. } | Event::TimerFired { .. } | Event::TimerCancelled { .. } => {
            EventFamily::Timer
        }
        Event::SignalReceived { .. } => EventFamily::Signal,
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
    use aion_store::InMemoryStore;
    use futures::{StreamExt, stream};
    use serde_json::json;

    use crate::durability::Recorder;
    use crate::registry::{CompletionNotifier, HandleResidency, WorkflowHandleParts};
    use crate::{
        LoadedWorkflows, Registry, RuntimeConfig, RuntimeHandle, SupervisionTree, WorkflowHandle,
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
        fn subscribe(&self, filter: EventFilter) -> BoxStream<'static, Event> {
            let events = self
                .events
                .iter()
                .filter(|event| filter.matches(event))
                .cloned()
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
        Ok(Engine::new(
            Arc::new(InMemoryStore::default()),
            RuntimeHandle::new(RuntimeConfig::new(Some(1)))?,
            LoadedWorkflows::new(),
            Registry::default(),
            SupervisionTree::new(),
            DelegatedSeams::new(signal_router, query_service, event_publisher),
        ))
    }

    async fn insert_active_handle(
        engine: &Engine,
    ) -> Result<WorkflowHandle, Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let store = engine.store();
        let mut recorder = Recorder::new(workflow_id.clone(), Arc::clone(&store));
        recorder
            .record_workflow_started(chrono::Utc::now(), "checkout".to_owned(), payload("input")?)
            .await?;
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid: engine.runtime().spawn_test_process_with_trap_exit(true)?,
            workflow_type: "checkout".to_owned(),
            loaded_version: ContentHash::from_bytes([1; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        engine
            .registry()
            .insert((workflow_id, run_id), handle.clone())?;
        Ok(handle)
    }

    fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        }
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
            parent_run_id: None,
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

        assert_eq!(events, vec![matching]);
        engine.shutdown()?;
        Ok(())
    }
}
