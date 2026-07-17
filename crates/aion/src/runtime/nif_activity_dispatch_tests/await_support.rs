use super::support::*;

/// Everything one `await_activity_step` determinism test needs over a
/// synthesized history.
pub(in super::super) struct AwaitHarness {
    pub(super) state: Arc<EngineNifState>,
    pub(super) registry: Arc<Registry>,
    pub(super) runtime: Arc<RuntimeHandle>,
    pub(super) store: Arc<dyn EventStore>,
    pub(super) workflow_id: WorkflowId,
    pub(super) pid: u64,
}

impl AwaitHarness {
    /// Build a fresh engine epoch (registry, handle, runtime) over an
    /// existing seeded store — the unit-level analogue of an engine
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
            workflow_type: "awaiter".to_owned(),
            namespace: String::from("default"),
            loaded_version: ContentHash::from_bytes([7; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id.clone(), run_id), handle)?;
        Ok(Self {
            state: Arc::new(EngineNifState::default()),
            registry,
            runtime,
            store,
            workflow_id,
            pid,
        })
    }

    /// One production-shaped pass: a fresh `NifContext` (one history
    /// read — the resolution snapshot) resolving the ordinal-0 await.
    pub(super) fn step(&self) -> Result<ActivityAwaitStep, String> {
        self.step_typed().map_err(|error| error.to_string())
    }

    pub(super) fn step_typed(&self) -> Result<ActivityAwaitStep, EngineError> {
        // Production runs this on a beamr scheduler thread with no
        // ambient Tokio context; block_in_place mirrors that so the
        // step's history reads can block_on the harness runtime.
        tokio::task::block_in_place(|| {
            let mut context = crate::runtime::nif_context::NifContext::new(
                self.pid,
                self.registry.as_ref(),
                tokio::runtime::Handle::current(),
                SignalDeliveryConfig::default(),
            )
            .map_err(|error| EngineError::Runtime {
                reason: error.error_reason(),
            })?;
            await_activity_step(
                &self.state,
                &mut context,
                &self.runtime,
                &ActivityId::from_sequence_position(0),
                || {},
            )
        })
    }

    /// Arm the per-test timer bridge that backed the OLD fresh-read
    /// expiry path (`expired_scope_message` → `build_context_for_pid`);
    /// installing it proves the stale-snapshot test fails if a fresh
    /// read is reintroduced, instead of accidentally passing because
    /// the fresh read was unavailable.
    pub(super) fn install_fresh_read_bridge(&self) {
        crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
            &self.state,
            Arc::clone(&self.registry),
            Arc::clone(&self.store),
            tokio::runtime::Handle::current(),
            SignalDeliveryConfig::default(),
        );
    }

    pub(super) fn arm_live_scope(&self, deadline_ordinal: u64) {
        self.state.timeout_scopes.insert(
            31,
            TimeoutScope::live_for_test(self.pid, aion_core::TimerId::anonymous(deadline_ordinal)),
        );
        self.state.timeout_scope_stacks.insert(self.pid, vec![31]);
    }

    pub(super) fn arm_replayed_expired_scope(&self, deadline_ordinal: u64) {
        self.state.timeout_scopes.insert(
            1,
            TimeoutScope::replayed_expired_with_deadline_for_test(
                self.pid,
                aion_core::TimerId::anonymous(deadline_ordinal),
            ),
        );
        self.state.timeout_scope_stacks.insert(self.pid, vec![1]);
    }

    pub(super) async fn history_len(&self) -> Result<usize, Box<dyn std::error::Error>> {
        Ok(self.store.read_history(&self.workflow_id).await?.len())
    }

    pub(super) fn shutdown(self) -> TestResult {
        self.runtime.shutdown()?;
        Ok(())
    }
}

pub(in super::super) fn envelope(workflow_id: &WorkflowId, seq: u64) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: chrono::Utc::now(),
        workflow_id: workflow_id.clone(),
    }
}

/// Seed `WorkflowStarted` + a scheduled/started ordinal-0 activity +
/// the scope deadline's `TimerFired` (seq 4).
pub(in super::super) async fn seed_pending_activity_then_deadline(
    store: &Arc<dyn EventStore>,
    deadline_ordinal: u64,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let events = vec![
        Event::WorkflowStarted {
            envelope: envelope(&workflow_id, 1),
            workflow_type: "awaiter".to_owned(),
            input: Payload::from_json(&json!({}))?,
            run_id: run_id.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
        Event::ActivityScheduled {
            envelope: envelope(&workflow_id, 2),
            activity_id: ActivityId::from_sequence_position(0),
            activity_type: "work".to_owned(),
            input: Payload::new(ContentType::Json, br#""in""#.to_vec()),
            task_queue: String::from("default"),
            node: None,
        },
        Event::ActivityStarted {
            envelope: envelope(&workflow_id, 3),
            activity_id: ActivityId::from_sequence_position(0),
            attempt: 1,
        },
        Event::TimerFired {
            envelope: envelope(&workflow_id, 4),
            timer_id: aion_core::TimerId::anonymous(deadline_ordinal),
        },
    ];
    store
        .append(WriteToken::recorder(), &workflow_id, &events, 0)
        .await?;
    Ok((workflow_id, run_id))
}
