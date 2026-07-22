use super::{events::*, support::*};

/// Fan-out members dispatch remotely: only an explicit in-VM selection is
/// refused; every other tier value (absence, null, remote tiers, junk)
/// passes through untouched.
#[test]
fn only_an_explicit_in_vm_selection_is_refused_from_fan_out() -> TestResult {
    let spec = |config: &str| -> Result<ActivitySpec, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"name":"member","input":"\"in\"","config":"{}"}}"#,
            config.replace('"', "\\\"")
        ))
    };
    assert!(spec(r#"{"tier":"in_vm"}"#)?.selects_in_vm());
    assert!(!spec(r#"{"tier":"remote_rust"}"#)?.selects_in_vm());
    assert!(!spec(r#"{"tier":null}"#)?.selects_in_vm());
    assert!(!spec(r#"{"labels":{}}"#)?.selects_in_vm());
    Ok(())
}

/// A dispatcher whose async dispatch never resolves: the schedule path
/// records `ActivityScheduled`+`ActivityStarted` durably, spawns the
/// completion task, then parks — so the recorded events are observable
/// without a completion racing the assertion.
pub(in super::super) struct NeverDispatcher;

impl crate::activity::bridge::ActivityDispatcher for NeverDispatcher {
    fn dispatch(&self, _request: ActivityDispatch) -> Result<String, String> {
        // Never invoked: dispatch_async is overridden to a pending future.
        Err("NeverDispatcher: dispatch_async never resolves".to_owned())
    }

    fn dispatch_async(
        self: Arc<Self>,
        _request: ActivityDispatch,
    ) -> futures::future::BoxFuture<'static, Result<String, String>> {
        Box::pin(std::future::pending())
    }
}

/// Everything one `collect_step` test needs over a synthesized history.
pub(in super::super) struct CollectHarness {
    pub(super) state: Arc<EngineNifState>,
    pub(super) deps: CollectDeps,
    pub(super) store: Arc<dyn EventStore>,
    pub(super) workflow_id: WorkflowId,
    pub(super) handle: WorkflowHandle,
    pub(super) pid: u64,
}

/// Seed `store` with `WorkflowStarted` + `events`, renumbered
/// contiguously from seq 1, and return the minted identifiers.
pub(in super::super) async fn seed_history(
    store: &Arc<dyn EventStore>,
    events: &[Event],
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let mut seeded = vec![started_event(&workflow_id, &run_id)?];
    seeded.extend(events.iter().cloned());
    let mut sequenced = Vec::with_capacity(seeded.len());
    for (index, event) in seeded.into_iter().enumerate() {
        let seq = u64::try_from(index)? + 1;
        sequenced.push(reenvelope(event, &workflow_id, seq));
    }
    store
        .append(WriteToken::recorder(), &workflow_id, &sequenced, 0)
        .await?;
    Ok((workflow_id, run_id))
}

impl CollectHarness {
    /// Build over a fresh store seeded with `WorkflowStarted` + `events`.
    pub(super) async fn over_events(events: &[Event]) -> Result<Self, Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let (workflow_id, run_id) = seed_history(&store, events).await?;
        Self::over_store(store, workflow_id, run_id).await
    }

    /// Build a fresh engine epoch (registry, handle, ordinal counters)
    /// over an existing store — the unit-level analogue of an engine
    /// restart before replay.
    pub(super) async fn over_store(
        store: Arc<dyn EventStore>,
        workflow_id: WorkflowId,
        run_id: RunId,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let head = u64::try_from(store.read_history(&workflow_id).await?.len())?;
        let registry = Arc::new(Registry::default());
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let pid = runtime.spawn_test_process()?;
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), head);
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "collect-parent".to_owned(),
            namespace: String::from("default"),
            loaded_version: ContentHash::from_bytes([5; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id.clone(), run_id), handle.clone())?;
        let deps = CollectDeps {
            registry,
            runtime: Arc::clone(&runtime),
            tokio_handle: tokio::runtime::Handle::current(),
            dispatcher: None,
        };
        Ok(Self {
            state: Arc::new(EngineNifState::default()),
            deps,
            store,
            workflow_id,
            handle,
            pid,
        })
    }

    pub(super) fn step(
        &self,
        kind: CollectKind,
        specs: &[ActivitySpec],
    ) -> Result<CollectStep, String> {
        // Production runs this on a beamr scheduler thread with no
        // ambient Tokio context; block_in_place mirrors that so the
        // step's history reads can block_on the harness runtime.
        tokio::task::block_in_place(|| {
            collect_step(&self.state, &self.deps, self.pid, kind, specs, "collect")
                .map_err(|error| error.to_string())
        })
    }

    pub(super) fn pinned(&self) -> Option<PendingAwait> {
        self.state.pending_awaits.get(&self.pid).map(|e| e.clone())
    }

    /// Every recorded `ActivityScheduled` as `(ordinal, task_queue)`, in
    /// history order — the durable record the fresh dispatch stamped.
    pub(super) async fn scheduled_task_queues(
        &self,
    ) -> Result<Vec<(u64, String)>, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .read_history(&self.workflow_id)
            .await?
            .iter()
            .filter_map(|event| match event {
                Event::ActivityScheduled {
                    activity_id,
                    task_queue,
                    ..
                } => Some((activity_id.sequence_position(), task_queue.clone())),
                _ => None,
            })
            .collect())
    }

    /// Every recorded `ActivityScheduled` as `(ordinal, node)`, in history
    /// order — the durable OPTIONAL affinity the fresh dispatch stamped.
    pub(super) async fn scheduled_nodes(
        &self,
    ) -> Result<Vec<(u64, Option<String>)>, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .read_history(&self.workflow_id)
            .await?
            .iter()
            .filter_map(|event| match event {
                Event::ActivityScheduled {
                    activity_id, node, ..
                } => Some((activity_id.sequence_position(), node.clone())),
                _ => None,
            })
            .collect())
    }

    pub(super) async fn cancelled_ordinals(&self) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .read_history(&self.workflow_id)
            .await?
            .iter()
            .filter_map(|event| match event {
                Event::ActivityCancelled { activity_id, .. } => {
                    Some(activity_id.sequence_position())
                }
                _ => None,
            })
            .collect())
    }

    pub(super) fn shutdown(self) -> TestResult {
        self.deps.runtime.shutdown()?;
        Ok(())
    }
}
