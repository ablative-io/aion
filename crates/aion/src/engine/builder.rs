//! `EngineBuilder` and build wiring.

use std::{path::PathBuf, sync::Arc};

use aion_core::{Event, status_from_events};
use aion_package::Package;
use aion_store::visibility::VisibilityStore;
use aion_store::{EventStore, InMemoryStore};

use crate::{
    CompletionNotifier, EngineError, HandleResidency, LoadedWorkflows, Registry, RuntimeConfig,
    RuntimeHandle, SupervisionTree, WorkflowHandle, WorkflowHandleParts,
    activity::bridge::{ActivityDispatcher, install_activity_dispatcher},
    durability::{ActiveWorkflowRecoverySeam, DeferredActiveWorkflowRecovery, Recorder},
    runtime::{NifEntry, NifRegistration},
};

use super::api::Engine;
use super::delegated::{DelegatedSeams, EventPublisher, QueryService, SignalRouter};

/// Source for a workflow package collected before `build()` performs fallible
/// loading and runtime registration.
#[derive(Clone, Debug)]
pub enum WorkflowPackageSource {
    /// Load a package from this `.aion` archive path during `build()`.
    Path(PathBuf),
    /// Use an already-loaded package value.
    Package(Box<Package>),
}

impl From<Package> for WorkflowPackageSource {
    fn from(package: Package) -> Self {
        Self::Package(Box::new(package))
    }
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
    workflow_sources: Vec<WorkflowPackageSource>,
    host_nifs: Vec<NifEntry>,
    recovery: Arc<dyn ActiveWorkflowRecoverySeam>,
    delegated: DelegatedSeams,
    activity_dispatcher: Option<Arc<dyn ActivityDispatcher>>,
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
            workflow_sources: Vec::new(),
            host_nifs: Vec::new(),
            recovery: Arc::new(DeferredActiveWorkflowRecovery),
            delegated: DelegatedSeams::default(),
            activity_dispatcher: None,
        }
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

    /// Record the caller-supplied scheduler thread count.
    ///
    /// If this setter is never called, `None` is passed through to beamr.
    #[must_use]
    pub const fn scheduler_threads(mut self, threads: usize) -> Self {
        self.scheduler_threads = Some(threads);
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
        self.recovery = recovery;
        self
    }

    /// Override the AT signal-routing seam.
    #[must_use]
    pub fn signal_router(mut self, signal_router: Arc<dyn SignalRouter>) -> Self {
        self.delegated = DelegatedSeams::new(
            signal_router,
            self.delegated.query_service_arc(),
            self.delegated.event_publisher_arc(),
        );
        self
    }

    /// Override the AT query-dispatch seam.
    #[must_use]
    pub fn query_service(mut self, query_service: Arc<dyn QueryService>) -> Self {
        self.delegated = DelegatedSeams::new(
            self.delegated.signal_router_arc(),
            query_service,
            self.delegated.event_publisher_arc(),
        );
        self
    }

    /// Override the AD/AT live event-publisher seam.
    #[must_use]
    pub fn event_publisher(mut self, event_publisher: Arc<dyn EventPublisher>) -> Self {
        self.delegated = DelegatedSeams::new(
            self.delegated.signal_router_arc(),
            self.delegated.query_service_arc(),
            event_publisher,
        );
        self
    }

    /// Supply the activity dispatcher that backs `aion_flow_ffi:run_activity`.
    ///
    /// When set, the dispatcher is installed in the global bridge before
    /// workflow modules are loaded. Without a dispatcher, `run_activity`
    /// returns an error to workflow code instead of crashing the process.
    #[must_use]
    pub fn activity_dispatcher(mut self, dispatcher: Arc<dyn ActivityDispatcher>) -> Self {
        self.activity_dispatcher = Some(dispatcher);
        self
    }

    /// Inspect the configured scheduler thread count.
    #[must_use]
    pub const fn scheduler_thread_count(&self) -> Option<usize> {
        self.scheduler_threads
    }

    /// Construct the live engine.
    ///
    /// # Errors
    ///
    /// Returns typed [`EngineError`] variants for missing store, runtime startup,
    /// NIF registration, package loading, store reads, registry/supervision lock
    /// poison, or deferred AD recovery failures for active histories.
    pub async fn build(self) -> Result<Engine, EngineError> {
        let store = self.store.ok_or(EngineError::MissingStore)?;
        let visibility_store = self
            .visibility_store
            .unwrap_or_else(|| Arc::new(InMemoryStore::default()));

        if let Some(dispatcher) = self.activity_dispatcher {
            install_activity_dispatcher(dispatcher);
        }

        let runtime = RuntimeHandle::new(RuntimeConfig::new(self.scheduler_threads))?;

        let mut nifs = NifRegistration::new();
        nifs.add_engine_nifs().add_host_nifs(self.host_nifs);
        runtime.install_nifs(nifs)?;

        let mut loaded_workflows = LoadedWorkflows::new();
        for source in self.workflow_sources {
            let package = package_from_source(source)?;
            loaded_workflows.load_package(&runtime, &package)?;
        }

        let registry = Registry::default();
        let supervision = SupervisionTree::new();
        repopulate_active_workflows(
            Arc::clone(&store),
            Arc::clone(&visibility_store),
            &loaded_workflows,
            &registry,
            &supervision,
            self.recovery.as_ref(),
        )
        .await?;

        Ok(Engine::new(
            store,
            visibility_store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
            self.delegated,
        ))
    }
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

async fn repopulate_active_workflows(
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    loaded_workflows: &LoadedWorkflows,
    registry: &Registry,
    supervision: &SupervisionTree,
    recovery: &dyn ActiveWorkflowRecoverySeam,
) -> Result<(), EngineError> {
    for workflow_id in store.as_ref().list_active().await? {
        let history = store.as_ref().read_history(&workflow_id).await?;
        let workflow_type = started_workflow_type(&workflow_id, &history)?;
        let projected_status = status_from_events(&history);
        supervision.ensure_type_supervisor(workflow_type.clone())?;

        let recovered = recovery.recover_active_workflow(
            &workflow_id,
            &workflow_type,
            &history,
            loaded_workflows,
        )?;
        let history_len = u64::try_from(history.len()).unwrap_or(u64::MAX);
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), history_len)
            .with_visibility(recovered.run_id.clone(), Arc::clone(&visibility_store));
        let completion = CompletionNotifier::new();
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: recovered.run_id.clone(),
            pid: recovered.pid,
            workflow_type: workflow_type.clone(),
            loaded_version: recovered.loaded_version,
            cached_status: projected_status,
            residency: HandleResidency::Resident,
            recorder,
            completion,
        });
        registry.insert((workflow_id.clone(), recovered.run_id.clone()), handle)?;
        registry.reconcile(&workflow_id, &recovered.run_id, &history)?;
        supervision.place_workflow(workflow_type, recovered.pid)?;
    }

    Ok(())
}

fn started_workflow_type(
    workflow_id: &aion_core::WorkflowId,
    history: &[Event],
) -> Result<String, EngineError> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted { workflow_type, .. } => Some(workflow_type.clone()),
            _ => None,
        })
        .ok_or_else(|| EngineError::Load {
            reason: format!(
                "active workflow `{workflow_id}` has no WorkflowStarted event in durable history"
            ),
        })
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};

    use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus};
    use aion_package::{
        BeamModule, BeamSet, CURRENT_FORMAT_VERSION, ContentHash, DeclaredActivity, Manifest,
        ManifestVersion, Package, PackageBuilder,
    };
    use aion_store::{EventStore, InMemoryStore};
    use chrono::Utc;
    use serde_json::json;

    use crate::durability::{ActiveWorkflowRecovery, ActiveWorkflowRecoverySeam};
    use crate::runtime::{Mfa, NifEntry};

    use super::{EngineBuilder, LoadedWorkflows};
    use crate::{EngineError, Pid};

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
            parent_run_id: None,
        })
    }

    fn hash(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
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

    #[derive(Debug)]
    struct TestRecovery {
        run_id: RunId,
        version: ContentHash,
        pid: Pid,
    }

    impl ActiveWorkflowRecoverySeam for TestRecovery {
        fn recover_active_workflow(
            &self,
            workflow_id: &WorkflowId,
            workflow_type: &str,
            history: &[Event],
            loaded_workflows: &LoadedWorkflows,
        ) -> Result<ActiveWorkflowRecovery, EngineError> {
            let _ = (workflow_id, workflow_type, history, loaded_workflows);
            Ok(ActiveWorkflowRecovery {
                run_id: self.run_id.clone(),
                loaded_version: self.version.clone(),
                pid: self.pid,
            })
        }
    }

    #[tokio::test]
    async fn build_without_store_returns_missing_store() {
        let error = EngineBuilder::new().build().await.err();

        assert!(matches!(error, Some(EngineError::MissingStore)));
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

    #[tokio::test]
    async fn duplicate_host_nif_mfa_returns_typed_error() {
        let mfa = Mfa::new("host", "zero", 0);
        let error = EngineBuilder::new()
            .store(InMemoryStore::default())
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
    async fn empty_store_builds_empty_registry_and_no_type_supervisors() -> Result<(), EngineError>
    {
        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .build()
            .await?;

        assert!(engine.registry().list()?.is_empty());
        assert_eq!(engine.supervision().type_supervisor_count()?, 0);
        assert_eq!(engine.loaded_workflows().iter().count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn build_loads_already_loaded_package() -> Result<(), Box<dyn std::error::Error>> {
        let package = fixture_package()?;
        let version = package.content_hash().clone();
        let deployed_entry_module = package.deployed_entry_module();

        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
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
    async fn build_loads_package_from_path() -> Result<(), Box<dyn std::error::Error>> {
        let package = fixture_package()?;
        let version = package.content_hash().clone();
        let path = write_fixture_package(&package)?;

        let engine = EngineBuilder::new()
            .store(InMemoryStore::default())
            .load_workflows(path.as_path())
            .build()
            .await?;
        std::fs::remove_file(path)?;

        assert!(engine.loaded_workflows().get("counter", &version).is_some());
        Ok(())
    }

    #[tokio::test]
    async fn active_workflow_recovery_repopulates_registry_and_supervision()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = InMemoryStore::default();
        let workflow_id = WorkflowId::new_v4();
        store
            .append(&workflow_id, &[started(&workflow_id, "checkout")?], 0)
            .await?;
        let run_id = RunId::new_v4();
        let version = hash(7);
        let pid = 42;

        let engine = EngineBuilder::new()
            .store(store)
            .recovery_seam(Arc::new(TestRecovery {
                run_id: run_id.clone(),
                version: version.clone(),
                pid,
            }))
            .build()
            .await?;

        let recovered = engine.registry().get(&workflow_id, &run_id)?;
        assert!(recovered.is_some_and(|handle| {
            handle.workflow_type() == "checkout"
                && handle.loaded_version() == &version
                && handle.cached_status() == WorkflowStatus::Running
                && handle.pid() == pid
        }));
        assert_eq!(engine.supervision().type_supervisor_count()?, 1);
        assert!(
            engine
                .supervision()
                .type_supervisors()?
                .iter()
                .any(|node| node.id().workflow_type() == "checkout")
        );
        Ok(())
    }
}
