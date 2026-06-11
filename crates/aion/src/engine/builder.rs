//! `EngineBuilder` and build wiring.

use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, time::Duration};

use chrono::Utc;

use aion_core::SearchAttributeSchema;
use aion_package::Package;
use aion_store::visibility::VisibilityStore;
use aion_store::{EventStore, InMemoryStore};

use crate::{
    EngineError, LoadedWorkflows, Registry, RuntimeConfig, RuntimeHandle, SignalDeliveryConfig,
    SupervisionTree,
    activity::bridge::ActivityDispatcher,
    durability::ActiveWorkflowRecoverySeam,
    runtime::{
        ChildNifBridge, ChildNifBridgeParts, NifEntry, NifRegistration, install_child_nif_bridge,
        install_nif_runtime_context, install_query_bridge, install_signal_nif_bridge,
        nif_determinism::{NifContextSource, install_nif_context_source},
    },
    signal::SignalResumeHandoff,
};

use super::api::{Engine, EngineComponents};
use super::delegated::{DelegatedSeams, EventPublisher, QueryService, SignalRouter};
use super::seams::{
    SeamAssembly, SignalRouterFactory, assemble_delegated_seams, wrap_event_streaming,
};
use super::startup::{
    StartupRecoveryContext, recover_active_workflows_on_startup, recover_timers_on_startup,
};

/// Source for a workflow package collected before `build()` performs fallible
/// loading and runtime registration.
#[derive(Clone, Debug)]
pub enum WorkflowPackageSource {
    /// Load a package from this `.aion` archive path during `build()`.
    Path(PathBuf),
    /// Use an already-loaded package value.
    Package(Box<Package>),
}

/// Install the engine-scoped NIF seams that are available before delegated
/// seams exist: runtime context, timer bridge, deterministic context source,
/// query bridge, and the optional activity dispatcher.
///
/// Returns the query mailbox engine handle installed in the query bridge, so
/// `build()` can wire the concrete query-dispatch seam over the same
/// delivery path the NIF-side `dispatch_query` uses.
fn install_engine_nif_seams(
    nif_state: &Arc<crate::runtime::EngineNifState>,
    registry: &Arc<Registry>,
    store: &Arc<dyn EventStore>,
    runtime: &Arc<RuntimeHandle>,
    activity_dispatcher: Option<Arc<dyn ActivityDispatcher>>,
    query_timeout: Option<Duration>,
) -> Arc<dyn crate::engine_seam::EngineHandle> {
    install_nif_runtime_context(
        nif_state,
        Arc::clone(registry),
        Arc::clone(runtime),
        tokio::runtime::Handle::current(),
    );
    crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
        nif_state,
        Arc::clone(registry),
        Arc::clone(store),
        tokio::runtime::Handle::current(),
        runtime.signal_delivery(),
    );
    install_nif_context_source(
        nif_state,
        Arc::new(NifContextSource::new(
            Arc::clone(registry),
            tokio::runtime::Handle::current(),
            Arc::clone(store),
            runtime.signal_delivery(),
        )),
    );
    let query_mailbox_engine = install_query_bridge(
        nif_state,
        Arc::clone(registry),
        runtime,
        tokio::runtime::Handle::current(),
        query_timeout,
    );
    if let Some(dispatcher) = activity_dispatcher {
        nif_state.set_activity_dispatcher(dispatcher);
    }
    query_mailbox_engine
}

fn load_workflow_sources(
    runtime: &RuntimeHandle,
    sources: Vec<WorkflowPackageSource>,
) -> Result<LoadedWorkflows, EngineError> {
    let mut loaded_workflows = LoadedWorkflows::new();
    for source in sources {
        let package = package_from_source(source)?;
        let loaded_workflow = loaded_workflows.load_package(runtime, &package)?;
        tracing::info!(
            workflow_type = loaded_workflow.workflow_type(),
            content_hash = %loaded_workflow.version(),
            "loaded workflow package {}",
            loaded_workflow.workflow_type()
        );
    }
    Ok(loaded_workflows)
}

impl From<Package> for WorkflowPackageSource {
    fn from(package: Package) -> Self {
        Self::Package(Box::new(package))
    }
}

fn spawn_visibility_reconciliation_task(
    interval: Duration,
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(error) = crate::lifecycle::visibility::reconcile_visibility(
                Arc::clone(&store),
                Arc::clone(&visibility_store),
            )
            .await
            {
                tracing::warn!(
                    error = %error,
                    "periodic visibility reconciliation failed; crash-consistency window may remain until a later reconciliation repairs visibility"
                );
            }
        }
    })
}

impl From<PathBuf> for WorkflowPackageSource {
    fn from(path: PathBuf) -> Self {
        Self::Path(path)
    }
}

impl From<&std::path::Path> for WorkflowPackageSource {
    fn from(path: &std::path::Path) -> Self {
        Self::Path(path.to_path_buf())
    }
}

impl From<&str> for WorkflowPackageSource {
    fn from(path: &str) -> Self {
        Self::Path(PathBuf::from(path))
    }
}

impl From<String> for WorkflowPackageSource {
    fn from(path: String) -> Self {
        Self::Path(PathBuf::from(path))
    }
}

/// Builder for the embedded, transport-agnostic workflow engine.
pub struct EngineBuilder {
    store: Option<Arc<dyn EventStore>>,
    visibility_store: Option<Arc<dyn VisibilityStore>>,
    scheduler_threads: Option<usize>,
    signal_delivery: SignalDeliveryConfig,
    workflow_sources: Vec<WorkflowPackageSource>,
    host_nifs: Vec<NifEntry>,
    recovery: Option<Arc<dyn ActiveWorkflowRecoverySeam>>,
    delegated: DelegatedSeams,
    signal_router_factory: Option<SignalRouterFactory>,
    activity_dispatcher: Option<Arc<dyn ActivityDispatcher>>,
    active_registry: Option<Arc<Registry>>,
    visibility_reconciliation_interval: Option<Duration>,
    search_attribute_schema: SearchAttributeSchema,
    event_streaming_capacity: Option<NonZeroUsize>,
    event_publisher_overridden: bool,
    query_timeout: Option<Duration>,
    query_service_overridden: bool,
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineBuilder {
    /// Create a builder with no store, no scheduler-thread override, no loaded
    /// workflows, and no host NIFs.
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: None,
            visibility_store: None,
            scheduler_threads: None,
            signal_delivery: SignalDeliveryConfig::default(),
            workflow_sources: Vec::new(),
            host_nifs: Vec::new(),
            recovery: None,
            delegated: DelegatedSeams::default(),
            signal_router_factory: None,
            activity_dispatcher: None,
            active_registry: None,
            visibility_reconciliation_interval: None,
            search_attribute_schema: SearchAttributeSchema::new(),
            event_streaming_capacity: None,
            event_publisher_overridden: false,
            query_timeout: None,
            query_service_overridden: false,
        }
    }

    /// Record the caller-supplied workflow query reply timeout.
    ///
    /// Setting a timeout installs the concrete query-dispatch seam during
    /// `build()` (unless [`Self::query_service`] overrides it) and enables
    /// the in-engine `dispatch_query` NIF. There is no default: without this
    /// call the query seam stays deferred and `Engine::query` fails typed
    /// with its "not configured" error.
    #[must_use]
    pub const fn query_timeout(mut self, timeout: Duration) -> Self {
        self.query_timeout = Some(timeout);
        self
    }

    /// Inspect the configured workflow query reply timeout.
    #[must_use]
    pub const fn configured_query_timeout(&self) -> Option<Duration> {
        self.query_timeout
    }

    /// Opt in to live event streaming with a caller-provided broadcast capacity.
    ///
    /// `build()` wraps the configured store in a
    /// [`PublishingEventStore`](crate::publish::PublishingEventStore) before any
    /// recorder, recovery, or NIF bridge captures the store — so every
    /// successful append publishes — and installs the matching
    /// [`BroadcastEventPublisher`](crate::publish::BroadcastEventPublisher) as
    /// the event-publisher seam behind [`Engine::subscribe`]. Without this call
    /// the deferred publisher remains installed and subscriptions are empty.
    #[must_use]
    pub const fn event_streaming(mut self, capacity: NonZeroUsize) -> Self {
        self.event_streaming_capacity = Some(capacity);
        self
    }

    /// Supply the search attribute schema validating every recorded attribute.
    ///
    /// The default schema is empty, which rejects all search attributes: a
    /// deployment must declare each attribute name and type before workflows
    /// can record values for it.
    #[must_use]
    pub fn search_attribute_schema(mut self, schema: SearchAttributeSchema) -> Self {
        self.search_attribute_schema = schema;
        self
    }

    /// Supply the event store used by the engine.
    #[must_use]
    pub fn store<S>(mut self, store: S) -> Self
    where
        S: EventStore,
    {
        self.store = Some(Arc::new(store));
        self
    }

    /// Supply an already type-erased event store.
    #[must_use]
    pub fn store_arc(mut self, store: Arc<dyn EventStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Supply the visibility store used by the engine for workflow projections.
    #[must_use]
    pub fn visibility_store<S>(mut self, visibility_store: S) -> Self
    where
        S: VisibilityStore,
    {
        self.visibility_store = Some(Arc::new(visibility_store));
        self
    }

    /// Supply an already type-erased visibility store.
    #[must_use]
    pub fn visibility_store_arc(mut self, visibility_store: Arc<dyn VisibilityStore>) -> Self {
        self.visibility_store = Some(visibility_store);
        self
    }

    /// Explicitly opt in to an ephemeral in-memory visibility store.
    ///
    /// This is intended for tests and local scenarios that do not need durable
    /// visibility projections. Visibility data stored this way does not survive
    /// process restarts.
    #[must_use]
    pub fn in_memory_visibility(mut self) -> Self {
        self.visibility_store = Some(Arc::new(InMemoryStore::default()));
        self
    }

    /// Record the caller-supplied scheduler thread count.
    ///
    /// If this setter is never called, `None` is passed through to beamr.
    #[must_use]
    pub const fn scheduler_threads(mut self, threads: usize) -> Self {
        self.scheduler_threads = Some(threads);
        self
    }

    /// Record the caller-supplied periodic visibility reconciliation interval.
    ///
    /// If this setter is never called, no periodic background reconciliation task is spawned.
    #[must_use]
    pub const fn visibility_reconciliation_interval(mut self, interval: Duration) -> Self {
        self.visibility_reconciliation_interval = Some(interval);
        self
    }

    /// Record the caller-supplied signal delivery readiness and retry policy.
    #[must_use]
    pub const fn signal_delivery(mut self, signal_delivery: SignalDeliveryConfig) -> Self {
        self.signal_delivery = signal_delivery;
        self
    }

    /// Add one workflow package source to load during `build()`.
    #[must_use]
    pub fn load_workflows(mut self, source: impl Into<WorkflowPackageSource>) -> Self {
        self.workflow_sources.push(source.into());
        self
    }

    /// Add many workflow package sources to load during `build()`.
    #[must_use]
    pub fn load_workflow_sources<I, S>(mut self, sources: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<WorkflowPackageSource>,
    {
        self.workflow_sources
            .extend(sources.into_iter().map(Into::into));
        self
    }

    /// Collect host-supplied NIF entries to install before workflow modules load.
    #[must_use]
    pub fn register_nifs(mut self, entries: impl IntoIterator<Item = NifEntry>) -> Self {
        self.host_nifs.extend(entries);
        self
    }

    /// Override the AD recovery seam used while repopulating active workflows.
    #[must_use]
    pub fn recovery_seam(mut self, recovery: Arc<dyn ActiveWorkflowRecoverySeam>) -> Self {
        self.recovery = Some(recovery);
        self
    }

    /// Use the production AD recovery seam created after runtime/package loading.
    #[must_use]
    pub fn production_recovery_seam(mut self) -> Self {
        self.recovery = None;
        self
    }

    /// Override the AT signal-routing seam.
    #[must_use]
    pub fn signal_router(mut self, signal_router: Arc<dyn SignalRouter>) -> Self {
        self.signal_router_factory = None;
        self.delegated = DelegatedSeams::new(
            signal_router,
            self.delegated.query_service_arc(),
            self.delegated.event_publisher_arc(),
        );
        self
    }

    /// Override the AT signal-routing seam after the runtime is assembled.
    #[must_use]
    pub fn signal_router_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn(Arc<RuntimeHandle>, Arc<SignalResumeHandoff>) -> Arc<dyn SignalRouter>
            + Send
            + Sync
            + 'static,
    {
        self.signal_router_factory = Some(Arc::new(factory));
        self
    }

    /// Override the AT query-dispatch seam.
    ///
    /// An explicit override wins over the concrete service that
    /// [`Self::query_timeout`] would otherwise install during `build()`.
    #[must_use]
    pub fn query_service(mut self, query_service: Arc<dyn QueryService>) -> Self {
        self.query_service_overridden = true;
        self.delegated = DelegatedSeams::new(
            self.delegated.signal_router_arc(),
            query_service,
            self.delegated.event_publisher_arc(),
        );
        self
    }

    /// Override the AD/AT live event-publisher seam.
    ///
    /// Mutually exclusive with [`Self::event_streaming`], which installs the
    /// broadcast publisher itself; configuring both fails `build()`.
    #[must_use]
    pub fn event_publisher(mut self, event_publisher: Arc<dyn EventPublisher>) -> Self {
        self.event_publisher_overridden = true;
        self.delegated = DelegatedSeams::new(
            self.delegated.signal_router_arc(),
            self.delegated.query_service_arc(),
            event_publisher,
        );
        self
    }

    /// Supply the activity dispatcher that backs activity dispatch NIFs.
    ///
    /// When set, the dispatcher is installed in the global bridge before
    /// workflow modules are loaded. Without a dispatcher, `dispatch_activity`
    /// returns an error to workflow code instead of crashing the process.
    #[must_use]
    pub fn activity_dispatcher(mut self, dispatcher: Arc<dyn ActivityDispatcher>) -> Self {
        self.activity_dispatcher = Some(dispatcher);
        self
    }

    /// Supply the active workflow registry used by the built engine.
    ///
    /// Server-owned dispatchers that run behind raw NIFs use this to correlate a
    /// calling BEAM pid to the same workflow handle the engine registers.
    #[must_use]
    pub fn active_registry(mut self, registry: Arc<Registry>) -> Self {
        self.active_registry = Some(registry);
        self
    }

    /// Inspect the configured scheduler thread count.
    #[must_use]
    pub const fn scheduler_thread_count(&self) -> Option<usize> {
        self.scheduler_threads
    }

    /// Inspect the configured periodic visibility reconciliation interval.
    #[must_use]
    pub const fn configured_visibility_reconciliation_interval(&self) -> Option<Duration> {
        self.visibility_reconciliation_interval
    }

    /// Construct the live engine.
    ///
    /// # Errors
    ///
    /// Returns typed [`EngineError`] variants for missing store, runtime startup,
    /// NIF registration, package loading, store reads, registry/supervision lock
    /// poison, or deferred AD recovery failures for active histories.
    pub async fn build(self) -> Result<Engine, EngineError> {
        let (store, streaming_publisher) = wrap_event_streaming(
            self.store.ok_or(EngineError::MissingStore)?,
            self.event_streaming_capacity,
            self.event_publisher_overridden,
        )?;
        let visibility_store = self
            .visibility_store
            .ok_or(EngineError::MissingVisibilityStore)?;

        let runtime = Arc::new(RuntimeHandle::new(
            RuntimeConfig::new(self.scheduler_threads).with_signal_delivery(self.signal_delivery),
        )?);

        let mut nifs = NifRegistration::new();
        nifs.add_engine_nifs().add_host_nifs(self.host_nifs);
        runtime.install_nifs(nifs)?;

        let loaded_workflows = load_workflow_sources(runtime.as_ref(), self.workflow_sources)?;

        let registry = self
            .active_registry
            .unwrap_or_else(|| Arc::new(Registry::default()));
        let nif_state = Arc::clone(runtime.nif_state());
        let query_mailbox_engine = install_engine_nif_seams(
            &nif_state,
            &registry,
            &store,
            &runtime,
            self.activity_dispatcher,
            self.query_timeout,
        );
        let supervision = Arc::new(SupervisionTree::new());
        let search_attribute_schema = Arc::new(self.search_attribute_schema);
        let signal_handoff = Arc::new(SignalResumeHandoff::new());

        let delegated = assemble_delegated_seams(SeamAssembly {
            configured: self.delegated,
            signal_router_factory: self.signal_router_factory,
            runtime: Arc::clone(&runtime),
            signal_handoff: Arc::clone(&signal_handoff),
            streaming_publisher,
            query_mailbox_engine,
            query_timeout: self.query_timeout,
            query_service_overridden: self.query_service_overridden,
        });

        install_signal_nif_bridge(
            &nif_state,
            Arc::new(crate::runtime::SignalNifBridge::new(
                Arc::clone(&registry),
                Arc::clone(&runtime),
                tokio::runtime::Handle::current(),
                delegated.signal_router_arc(),
            )),
        );
        install_configured_child_nif_bridge(&ChildBridgeAssembly {
            nif_state: &nif_state,
            store: &store,
            visibility_store: &visibility_store,
            runtime: &runtime,
            loaded_workflows: &loaded_workflows,
            registry: &registry,
            supervision: &supervision,
            signal_handoff: &signal_handoff,
            search_attribute_schema: &search_attribute_schema,
            watch_backoff: self.signal_delivery,
        })?;

        // Startup recovery re-spawns active workflow processes, and those
        // processes begin replaying on scheduler threads immediately. Replay
        // re-executes workflow code through the engine NIFs, so every NIF
        // bridge (signal, child) must be installed before the first recovered
        // process can run, or an early replayed spawn_child/receive_signal
        // call fails with a missing-bridge error.
        recover_active_workflows_on_startup(StartupRecoveryContext {
            store: Arc::clone(&store),
            visibility_store: Arc::clone(&visibility_store),
            runtime: Arc::clone(&runtime),
            loaded_workflows: &loaded_workflows,
            registry: Arc::clone(&registry),
            supervision: Arc::clone(&supervision),
            recovery: self.recovery,
            search_attribute_schema: Arc::clone(&search_attribute_schema),
        })
        .await?;
        recover_timers_on_startup(&nif_state, Arc::clone(&store)).await?;

        let visibility_reconciliation_task =
            self.visibility_reconciliation_interval.map(|interval| {
                spawn_visibility_reconciliation_task(
                    interval,
                    Arc::clone(&store),
                    Arc::clone(&visibility_store),
                )
            });

        let engine = Engine::new(EngineComponents {
            store,
            visibility_store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
            delegated,
            signal_handoff,
            search_attribute_schema,
            visibility_reconciliation_task,
        });
        engine.catchup_schedule_coordinator().await?;
        engine.recover_schedules_on_startup(Utc::now()).await?;
        Ok(engine)
    }
}

/// Borrowed engine components assembled into the child NIF bridge.
struct ChildBridgeAssembly<'a> {
    nif_state: &'a Arc<crate::runtime::EngineNifState>,
    store: &'a Arc<dyn EventStore>,
    visibility_store: &'a Arc<dyn VisibilityStore>,
    runtime: &'a Arc<RuntimeHandle>,
    loaded_workflows: &'a LoadedWorkflows,
    registry: &'a Arc<Registry>,
    supervision: &'a Arc<SupervisionTree>,
    signal_handoff: &'a Arc<SignalResumeHandoff>,
    search_attribute_schema: &'a Arc<aion_core::SearchAttributeSchema>,
    /// The child-terminal watcher reuses the builder's delivery retry
    /// policy for its registry-miss backoff windows.
    watch_backoff: SignalDeliveryConfig,
}

fn install_configured_child_nif_bridge(
    assembly: &ChildBridgeAssembly<'_>,
) -> Result<(), EngineError> {
    install_child_nif_bridge(
        assembly.nif_state,
        Arc::new(ChildNifBridge::new(ChildNifBridgeParts {
            store: Arc::clone(assembly.store),
            visibility_store: Arc::clone(assembly.visibility_store),
            runtime: Arc::clone(assembly.runtime),
            loaded_workflows: assembly.loaded_workflows.clone(),
            registry: Arc::clone(assembly.registry),
            supervision: Arc::clone(assembly.supervision),
            signal_handoff: Arc::clone(assembly.signal_handoff),
            search_attribute_schema: Arc::clone(assembly.search_attribute_schema),
            tokio_handle: tokio::runtime::Handle::current(),
            watch_backoff: assembly.watch_backoff,
        })?),
    );
    Ok(())
}

fn package_from_source(source: WorkflowPackageSource) -> Result<Package, EngineError> {
    match source {
        WorkflowPackageSource::Path(path) => {
            Package::load_from_path(&path).map_err(|error| EngineError::Load {
                reason: format!(
                    "failed to load workflow package `{}`: {error}",
                    path.display()
                ),
            })
        }
        WorkflowPackageSource::Package(package) => Ok(*package),
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, path::PathBuf, process::Command, sync::Arc, time::Duration};

    use aion_core::{Event, EventEnvelope, Payload, WorkflowId, WorkflowStatus};
    use aion_package::{
        BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
        Package, PackageBuilder,
    };
    use aion_store::visibility::{ListWorkflowsFilter, VisibilityStore};
    use aion_store::{InMemoryStore, ReadableEventStore, WritableEventStore, WriteToken};
    use chrono::Utc;
    use futures::StreamExt;
    use serde_json::json;

    use crate::engine::api_schedule::{
        schedule_coordinator_run_id, schedule_coordinator_workflow_id,
        schedule_coordinator_workflow_type,
    };
    use crate::runtime::{Mfa, NifEntry};

    use super::EngineBuilder;
    use crate::EngineError;

    fn payload() -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "input": true }))
    }

    fn started(
        workflow_id: &WorkflowId,
        workflow_type: &str,
    ) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: workflow_type.to_owned(),
            input: payload()?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn completed(workflow_id: &WorkflowId) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowCompleted {
            envelope: EventEnvelope {
                seq: 2,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            result: payload()?,
        })
    }

    fn package_manifest() -> Manifest {
        Manifest {
            entry_module: "counter".to_owned(),
            entry_function: "version".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "integer" }),
            timeout: Duration::from_secs(30),
            activities: vec![DeclaredActivity {
                activity_type: "activity/test".to_owned(),
            }],
            version: ManifestVersion::new("test"),
            format_version: CURRENT_FORMAT_VERSION,
        }
    }

    fn compile_counter_beam() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let temp_dir =
            std::env::temp_dir().join(format!("aion-engine-builder-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temp_dir)?;
        let source_path = temp_dir.join("counter.erl");
        let beam_path = temp_dir.join("counter.beam");
        std::fs::write(
            &source_path,
            "-module(counter).\n-export([version/0]).\nversion() -> 1.\n",
        )?;
        let status = Command::new("erlc")
            .arg("-o")
            .arg(&temp_dir)
            .arg(&source_path)
            .status()?;
        if !status.success() {
            let cleanup_result = std::fs::remove_dir_all(&temp_dir);
            drop(cleanup_result);
            return Err(format!("erlc failed with status {status}").into());
        }
        let bytes = std::fs::read(beam_path)?;
        std::fs::remove_dir_all(temp_dir)?;
        Ok(bytes)
    }

    fn fixture_package() -> Result<Package, Box<dyn std::error::Error>> {
        let beams = BeamSet::new(vec![BeamModule::new("counter", compile_counter_beam()?)])?;
        let archive = PackageBuilder::new(package_manifest(), beams).write_to_bytes()?;
        Ok(Package::load_from_bytes(archive)?)
    }

    fn write_fixture_package(package: &Package) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("aion-engine-builder-{}.aion", uuid::Uuid::new_v4()));
        PackageBuilder::new(package.manifest().clone(), package.beams().clone())
            .write_to_path(&path)?;
        Ok(path)
    }

    #[tokio::test]
    async fn build_without_store_returns_missing_store() {
        let error = EngineBuilder::new().build().await.err();

        assert!(matches!(error, Some(EngineError::MissingStore)));
    }

    #[tokio::test]
    async fn build_without_visibility_store_returns_missing_visibility_store() {
        let error = EngineBuilder::new()
            .store(InMemoryStore::default())
            .build()
            .await
            .err();

        assert!(matches!(error, Some(EngineError::MissingVisibilityStore)));
    }

    #[tokio::test]
    async fn in_memory_visibility_allows_build_without_visibility_store() -> Result<(), EngineError>
    {
        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .build()
            .await?;

        engine.shutdown()?;
        Ok(())
    }

    fn capacity(value: usize) -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
        NonZeroUsize::new(value).ok_or_else(|| "capacity must be non-zero".into())
    }

    #[tokio::test]
    async fn event_streaming_delivers_recorder_appends_through_engine_subscribe()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .event_streaming(capacity(8)?)
            .build()
            .await?;
        let workflow_id = WorkflowId::new_v4();
        let mut subscription = engine.subscribe(crate::EventFilter {
            workflow_id: Some(workflow_id.clone()),
            run: None,
            family: None,
        });

        // The production append path: a Recorder over the engine's store,
        // which `event_streaming` wrapped before any recorder existed.
        let mut recorder = crate::durability::Recorder::new(workflow_id.clone(), engine.store());
        recorder
            .record_workflow_started(
                Utc::now(),
                crate::durability::WorkflowStartRecord {
                    workflow_type: "checkout".to_owned(),
                    input: payload()?,
                    run_id: aion_core::RunId::new(uuid::Uuid::from_u128(7)),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;

        let item = tokio::time::timeout(Duration::from_secs(2), subscription.next())
            .await?
            .ok_or("subscription ended without delivering the appended event")?;
        let event = item?;
        assert_eq!(event.workflow_id(), &workflow_id);
        assert_eq!(event.seq(), 1);
        assert!(matches!(event, Event::WorkflowStarted { .. }));
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn without_event_streaming_subscriptions_stay_on_deferred_empty_stream()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .build()
            .await?;

        let mut subscription = engine.subscribe(crate::EventFilter::default());
        let item = tokio::time::timeout(Duration::from_secs(2), subscription.next()).await?;

        assert!(item.is_none(), "deferred publisher streams must be empty");
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn event_streaming_conflicts_with_explicit_event_publisher()
    -> Result<(), Box<dyn std::error::Error>> {
        let error = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .event_publisher(Arc::new(crate::DeferredEventPublisher))
            .event_streaming(capacity(8)?)
            .build()
            .await
            .err();

        assert!(matches!(
            error,
            Some(EngineError::ConflictingEventPublisher)
        ));
        Ok(())
    }

    #[test]
    fn query_timeout_is_only_set_by_caller() {
        assert_eq!(EngineBuilder::new().configured_query_timeout(), None);
        assert_eq!(
            EngineBuilder::new()
                .query_timeout(Duration::from_secs(3))
                .configured_query_timeout(),
            Some(Duration::from_secs(3))
        );
    }

    async fn insert_running_workflow(
        engine: &crate::Engine,
    ) -> Result<(WorkflowId, aion_core::RunId), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = crate::durability::Recorder::new(workflow_id.clone(), engine.store());
        recorder
            .record_workflow_started(
                Utc::now(),
                crate::durability::WorkflowStartRecord {
                    workflow_type: "checkout".to_owned(),
                    input: payload()?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;
        let handle = crate::registry::WorkflowHandle::new(crate::registry::WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid: engine.runtime().spawn_test_process_with_trap_exit(true)?,
            workflow_type: "checkout".to_owned(),
            loaded_version: aion_package::ContentHash::from_bytes([2; 32]),
            cached_status: WorkflowStatus::Running,
            residency: crate::registry::HandleResidency::Resident,
            recorder,
            completion: crate::registry::CompletionNotifier::new(),
        });
        engine
            .registry()
            .insert((workflow_id.clone(), run_id.clone()), handle)?;
        Ok((workflow_id, run_id))
    }

    #[tokio::test]
    async fn query_timeout_installs_the_concrete_query_seam()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .query_timeout(Duration::from_millis(250))
            .build()
            .await?;
        let (workflow_id, run_id) = insert_running_workflow(&engine).await?;

        // The concrete seam reached the query mailbox engine, which answers
        // an unregistered name with a typed UnknownQuery — the deferred seam
        // would have failed with its "not configured" runtime error instead.
        let result = engine.query(&workflow_id, &run_id, "state").await;

        assert!(matches!(
            result,
            Err(crate::EngineError::Query(crate::QueryError::UnknownQuery(name))) if name == "state"
        ));
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn without_query_timeout_the_query_seam_stays_deferred()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .build()
            .await?;
        let (workflow_id, run_id) = insert_running_workflow(&engine).await?;

        let result = engine.query(&workflow_id, &run_id, "state").await;

        assert!(matches!(
            result,
            Err(crate::EngineError::Runtime { reason }) if reason.contains("not configured")
        ));
        engine.shutdown()?;
        Ok(())
    }

    #[test]
    fn scheduler_threads_are_only_set_by_caller() {
        assert_eq!(EngineBuilder::new().scheduler_thread_count(), None);
        assert_eq!(
            EngineBuilder::new()
                .scheduler_threads(4)
                .scheduler_thread_count(),
            Some(4)
        );
    }

    #[test]
    fn visibility_reconciliation_interval_is_only_set_by_caller() {
        let interval = Duration::from_millis(250);

        assert_eq!(
            EngineBuilder::new().configured_visibility_reconciliation_interval(),
            None
        );
        assert_eq!(
            EngineBuilder::new()
                .visibility_reconciliation_interval(interval)
                .configured_visibility_reconciliation_interval(),
            Some(interval)
        );
    }

    #[tokio::test]
    async fn duplicate_host_nif_mfa_returns_typed_error() {
        let mfa = Mfa::new("host", "zero", 0);
        let error = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .register_nifs([
                NifEntry::new(mfa.clone(), crate::runtime::nif::test_native_zero),
                NifEntry::dirty(mfa, crate::runtime::nif::test_native_zero),
            ])
            .build()
            .await
            .err();

        assert!(matches!(
            error,
            Some(EngineError::NifRegistration { reason }) if reason.contains("host:zero/0")
        ));
    }

    #[tokio::test]
    async fn empty_store_builds_coordinator_history_without_registry_or_supervision()
    -> Result<(), EngineError> {
        let store = Arc::new(InMemoryStore::default());
        let engine = EngineBuilder::new()
            .store_arc(store.clone())
            .in_memory_visibility()
            .build()
            .await?;

        assert!(engine.registry().list()?.is_empty());
        assert_eq!(engine.supervision().type_supervisor_count()?, 1);
        assert_eq!(engine.loaded_workflows().iter().count(), 0);

        let coordinator_id = schedule_coordinator_workflow_id();
        let active = store.list_active().await?;
        assert_eq!(active, vec![coordinator_id.clone()]);
        let history = store.read_history(&coordinator_id).await?;
        let [started] = history.as_slice() else {
            return Err(EngineError::Load {
                reason: format!(
                    "expected exactly one coordinator event, found {}",
                    history.len()
                ),
            });
        };
        match started {
            Event::WorkflowStarted {
                workflow_type,
                input,
                run_id,
                parent_run_id,
                ..
            } => {
                assert_eq!(workflow_type, schedule_coordinator_workflow_type());
                assert_eq!(
                    input,
                    &Payload::from_json(&json!({})).map_err(|error| {
                        EngineError::Load {
                            reason: format!("failed to build expected payload: {error}"),
                        }
                    })?
                );
                assert_eq!(run_id, &schedule_coordinator_run_id());
                assert!(parent_run_id.is_none());
            }
            other => {
                return Err(EngineError::Load {
                    reason: format!("expected coordinator WorkflowStarted, found {other:?}"),
                });
            }
        }

        engine.shutdown()?;
        let rebuilt = EngineBuilder::new()
            .store_arc(store.clone())
            .in_memory_visibility()
            .build()
            .await?;
        let rebuilt_history = store.read_history(&coordinator_id).await?;
        assert_eq!(rebuilt_history.len(), 1);
        rebuilt.shutdown()?;

        Ok(())
    }

    #[tokio::test]
    async fn build_loads_already_loaded_package() -> Result<(), Box<dyn std::error::Error>> {
        let package = fixture_package()?;
        let version = package.content_hash().clone();
        let deployed_entry_module = package.deployed_entry_module();

        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .load_workflows(package)
            .build()
            .await?;

        let loaded = engine
            .loaded_workflows()
            .get("counter", &version)
            .ok_or("loaded package record missing")?;
        assert_eq!(loaded.deployed_entry_module(), deployed_entry_module);
        assert!(
            engine
                .runtime()
                .has_registered_module(&deployed_entry_module)
        );
        Ok(())
    }

    #[tokio::test]
    async fn startup_reconciliation_backfills_completed_visibility()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(InMemoryStore::default());
        let completed_id = WorkflowId::new_v4();

        store
            .append(
                WriteToken::recorder(),
                &completed_id,
                &[
                    started(&completed_id, "billing")?,
                    completed(&completed_id)?,
                ],
                0,
            )
            .await?;

        let engine = EngineBuilder::new()
            .store_arc(store.clone())
            .visibility_store_arc(store.clone())
            .build()
            .await?;

        let summaries = store.list_workflows(ListWorkflowsFilter::default()).await?;
        let completed_summary = summaries
            .iter()
            .find(|summary| summary.workflow_id == completed_id)
            .ok_or("completed workflow missing from visibility")?;

        assert_eq!(completed_summary.status, WorkflowStatus::Completed);
        assert!(completed_summary.close_time.is_some());
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn periodic_visibility_reconciliation_repairs_gap_after_startup()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(InMemoryStore::default());
        let engine = EngineBuilder::new()
            .store_arc(store.clone())
            .visibility_store_arc(store.clone())
            .visibility_reconciliation_interval(Duration::from_millis(25))
            .build()
            .await?;
        let workflow_id = WorkflowId::new_v4();

        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                &[started(&workflow_id, "checkout")?],
                0,
            )
            .await?;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let summaries = store.list_workflows(ListWorkflowsFilter::default()).await?;
                if summaries.iter().any(|summary| {
                    summary.workflow_id == workflow_id && summary.status == WorkflowStatus::Running
                }) {
                    return Ok::<(), aion_store::StoreError>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;

        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn build_loads_package_from_path() -> Result<(), Box<dyn std::error::Error>> {
        let package = fixture_package()?;
        let version = package.content_hash().clone();
        let path = write_fixture_package(&package)?;

        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .in_memory_visibility()
            .load_workflows(path.as_path())
            .build()
            .await?;
        std::fs::remove_file(path)?;

        assert!(engine.loaded_workflows().get("counter", &version).is_some());
        Ok(())
    }
}
