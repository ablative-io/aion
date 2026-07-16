//! Background spawn recovery for record-then-spawn child workflows (F3).
//!
//! `spawn_child` records `ChildWorkflowStarted` durably and then starts the
//! child. A start failure after the durable record must never surface to
//! workflow code: live would observe an error where replay (which resolves
//! the spawn from the recorded event) observes success — opposite branches.
//! Instead, the engine owns the start as an internal obligation: the NIF
//! returns the recorded child id, and a failed start is retried here with
//! the engine's backoff policy until the child's history exists or the
//! engine epoch ends (the task registry aborts every retry at shutdown; the
//! startup recovery sweep repairs the window in the next epoch).

use std::sync::Arc;

use aion_core::{PackageVersion, Payload, WorkflowId};

use crate::engine_seam::ChildWorkflowSpawnRequest;
use crate::runtime::nif_child_engine::ChildNifBridge;

/// Identity and payload of one recorded-but-not-yet-started child.
pub(super) struct RecordedChildSpawn {
    /// Parent that durably recorded the `ChildWorkflowStarted`.
    pub(super) parent_workflow_id: WorkflowId,
    /// Pre-allocated child identity carried by the recorded event.
    pub(super) child_workflow_id: WorkflowId,
    /// Recorded child workflow type.
    pub(super) workflow_type: String,
    /// Recorded child input payload.
    pub(super) input: Payload,
    /// Recorded child package version.
    pub(super) package_version: PackageVersion,
    /// Namespace inherited from the parent workflow.
    pub(super) namespace: String,
}

/// Arm a background task that starts the recorded child until it exists.
///
/// Idempotent per child id; refused once engine shutdown began (the startup
/// recovery sweep owns the repair in the next epoch). Returns whether a new
/// retry task was armed.
pub(super) fn ensure_child_started_in_background(
    bridge: &Arc<ChildNifBridge>,
    spawn: RecordedChildSpawn,
) -> bool {
    let task_bridge = Arc::clone(bridge);
    let child_id = spawn.child_workflow_id.clone();
    bridge
        .child_tasks()
        .arm_spawn_retry(child_id.clone(), async move {
            run_spawn_retry(&task_bridge, &spawn).await;
            task_bridge.child_tasks().remove_spawn_retry(&child_id);
        })
}

/// Retry the start until the child's durable history exists.
///
/// Store truth governs idempotence: a non-empty child history means the
/// child was started (by this task, by the synchronous attempt racing it,
/// or by a startup sweep) and the task is done. The start attempt runs
/// inline on the child-task runtime, so the F4 epoch close (abort *and*
/// await every task before releasing the runtime) covers it: once
/// `shutdown_child_tasks` returns, no retry attempt can append the child's
/// `WorkflowStarted` — a successor engine's startup sweep over the same
/// store can never race a zombie attempt from this epoch (N-4). The exit
/// monitors the start path installs capture the epoch-stable host handle
/// through `StartWorkflowContext::monitor_tokio_handle`, never this task's
/// own executor.
async fn run_spawn_retry(bridge: &Arc<ChildNifBridge>, spawn: &RecordedChildSpawn) {
    let policy = bridge.watch_backoff();
    let mut backoff = policy.initial_backoff;
    loop {
        match bridge.store().read_history(&spawn.child_workflow_id).await {
            Ok(history) if !history.is_empty() => return,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    child_workflow_id = %spawn.child_workflow_id,
                    error = %error,
                    "spawn recovery could not read the child history; backing off"
                );
                sleep_backoff(&mut backoff, &policy).await;
                continue;
            }
        }

        let request = ChildWorkflowSpawnRequest {
            parent_workflow_id: spawn.parent_workflow_id.clone(),
            child_workflow_id: spawn.child_workflow_id.clone(),
            workflow_type: spawn.workflow_type.clone(),
            input: spawn.input.clone(),
            package_version: spawn.package_version.clone(),
        };
        match bridge
            .start_child_under_recorded_id(&spawn.parent_workflow_id, &spawn.namespace, request)
            .await
            .map(|handle| handle.workflow_id().clone())
        {
            Ok(started_id) => {
                if started_id == spawn.child_workflow_id {
                    return;
                }
                // Engine invariant violation (F6): the start path must echo
                // the recorded identity. The loop re-reads store truth — if
                // nothing exists under the recorded id, the start is
                // attempted again; the wrongly-started execution is reported
                // loudly rather than silently adopted.
                tracing::error!(
                    parent_workflow_id = %spawn.parent_workflow_id,
                    recorded_child_workflow_id = %spawn.child_workflow_id,
                    started_workflow_id = %started_id,
                    "engine invariant violation: child start echoed a different workflow id"
                );
            }
            Err(error) => {
                tracing::warn!(
                    parent_workflow_id = %spawn.parent_workflow_id,
                    child_workflow_id = %spawn.child_workflow_id,
                    workflow_type = %spawn.workflow_type,
                    error = %error,
                    "recorded child start failed; retrying with backoff"
                );
            }
        }
        sleep_backoff(&mut backoff, &policy).await;
    }
}

async fn sleep_backoff(
    current: &mut std::time::Duration,
    policy: &crate::runtime::SignalDeliveryConfig,
) {
    tokio::time::sleep(*current).await;
    let doubled = current.saturating_mul(2);
    *current = if doubled > policy.max_backoff {
        policy.max_backoff
    } else {
        doubled
    };
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use aion_core::{ContentType, Payload, WorkflowId};
    use aion_store::{EventStore, InMemoryStore};

    use super::{RecordedChildSpawn, ensure_child_started_in_background};
    use crate::loader::WorkflowCatalog;
    use crate::registry::Registry;
    use crate::runtime::nif_child_engine::{ChildNifBridge, ChildNifBridgeParts};
    use crate::runtime::{RuntimeConfig, RuntimeHandle, SignalDeliveryConfig};
    use crate::signal::SignalResumeHandoff;
    use crate::supervision::SupervisionTree;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fast_backoff() -> SignalDeliveryConfig {
        SignalDeliveryConfig::new(
            Duration::ZERO,
            1,
            Duration::from_millis(1),
            Duration::from_millis(4),
        )
    }

    /// Bridge over an empty package set; requires a live Tokio runtime
    /// context for the captured handle.
    fn bridge_without_loaded_workflows()
    -> Result<(Arc<ChildNifBridge>, Arc<RuntimeHandle>), Box<dyn std::error::Error>> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let bridge = Arc::new(ChildNifBridge::new(ChildNifBridgeParts {
            store,
            visibility_store: backing,
            runtime: Arc::clone(&runtime),
            catalog: Arc::new(WorkflowCatalog::new()),
            registry: Arc::new(Registry::default()),
            supervision: Arc::new(SupervisionTree::new()),
            signal_handoff: Arc::new(SignalResumeHandoff::new()),
            search_attribute_schema: Arc::new(aion_core::SearchAttributeSchema::new()),
            tokio_handle: tokio::runtime::Handle::current(),
            watch_backoff: fast_backoff(),
        })?);
        Ok((bridge, runtime))
    }

    fn recorded_spawn(child: &WorkflowId) -> RecordedChildSpawn {
        RecordedChildSpawn {
            parent_workflow_id: WorkflowId::new_v4(),
            child_workflow_id: child.clone(),
            workflow_type: "never_loaded_child".to_owned(),
            input: Payload::new(ContentType::Json, br#""input""#.to_vec()),
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
            namespace: String::from("default"),
        }
    }

    /// Delegating store that parks every history read of one workflow on a
    /// notify and counts the reads currently parked; a parked read's guard
    /// decrements when its future is dropped, making task abort observable.
    struct GatedParentReadStore {
        inner: InMemoryStore,
        gated_workflow_id: WorkflowId,
        gate: tokio::sync::Notify,
        parked: Arc<AtomicU32>,
    }

    struct ParkGuard(Arc<AtomicU32>);

    impl Drop for ParkGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    #[async_trait::async_trait]
    impl aion_store::ReadableEventStore for GatedParentReadStore {
        async fn read_history(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Vec<aion_core::Event>, aion_store::StoreError> {
            if workflow_id == &self.gated_workflow_id {
                self.parked.fetch_add(1, Ordering::AcqRel);
                let _guard = ParkGuard(Arc::clone(&self.parked));
                self.gate.notified().await;
            }
            self.inner.read_history(workflow_id).await
        }

        async fn read_history_from(
            &self,
            workflow_id: &WorkflowId,
            from_seq: u64,
        ) -> Result<Vec<aion_core::Event>, aion_store::StoreError> {
            self.inner.read_history_from(workflow_id, from_seq).await
        }

        async fn read_run_chain(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Vec<aion_store::RunSummary>, aion_store::StoreError> {
            self.inner.read_run_chain(workflow_id).await
        }

        async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, aion_store::StoreError> {
            self.inner.list_workflow_ids().await
        }

        async fn list_active(&self) -> Result<Vec<WorkflowId>, aion_store::StoreError> {
            self.inner.list_active().await
        }

        async fn list_paused(&self) -> Result<Vec<WorkflowId>, aion_store::StoreError> {
            self.inner.list_paused().await
        }

        async fn query(
            &self,
            filter: &aion_core::WorkflowFilter,
        ) -> Result<Vec<aion_core::WorkflowSummary>, aion_store::StoreError> {
            self.inner.query(filter).await
        }

        async fn schedule_timer(
            &self,
            workflow_id: &WorkflowId,
            timer_id: &aion_core::TimerId,
            fire_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), aion_store::StoreError> {
            self.inner
                .schedule_timer(workflow_id, timer_id, fire_at)
                .await
        }

        async fn expired_timers(
            &self,
            as_of: chrono::DateTime<chrono::Utc>,
        ) -> Result<Vec<aion_store::TimerEntry>, aion_store::StoreError> {
            self.inner.expired_timers(as_of).await
        }
    }

    #[async_trait::async_trait]
    impl aion_store::WritableEventStore for GatedParentReadStore {
        async fn append(
            &self,
            token: aion_store::WriteToken,
            workflow_id: &WorkflowId,
            events: &[aion_core::Event],
            expected_seq: u64,
        ) -> Result<(), aion_store::StoreError> {
            self.inner
                .append(token, workflow_id, events, expected_seq)
                .await
        }
    }

    /// Package persistence is untouched by the read gate: forward to the
    /// wrapped in-memory store.
    #[async_trait::async_trait]
    impl aion_store::PackageStore for GatedParentReadStore {
        async fn put_package(
            &self,
            record: aion_store::PackageRecord,
        ) -> Result<(), aion_store::StoreError> {
            self.inner.put_package(record).await
        }

        async fn put_package_with_routes(
            &self,
            record: aion_store::PackageRecord,
            route_workflow_types: &[String],
        ) -> Result<(), aion_store::StoreError> {
            self.inner
                .put_package_with_routes(record, route_workflow_types)
                .await
        }

        async fn list_packages(
            &self,
        ) -> Result<Vec<aion_store::PackageRecord>, aion_store::StoreError> {
            self.inner.list_packages().await
        }

        async fn delete_package(
            &self,
            workflow_type: &str,
            content_hash: &str,
        ) -> Result<(), aion_store::StoreError> {
            self.inner.delete_package(workflow_type, content_hash).await
        }

        async fn put_package_route(
            &self,
            workflow_type: &str,
            content_hash: &str,
        ) -> Result<(), aion_store::StoreError> {
            self.inner
                .put_package_route(workflow_type, content_hash)
                .await
        }

        async fn list_package_routes(
            &self,
        ) -> Result<Vec<aion_store::PackageRouteRecord>, aion_store::StoreError> {
            self.inner.list_package_routes().await
        }
    }

    /// N-4: the start attempt must die with the epoch. The attempt is parked
    /// inside `start_child_under_recorded_id`'s parent-history read when the
    /// epoch closes; `shutdown_child_tasks` aborts AND awaits it, so by the
    /// time shutdown returns the attempt future is dropped (parked count 0)
    /// and releasing the gate afterwards resumes nothing — no append can
    /// land after the epoch closed. Before the fix the attempt ran on the
    /// HOST runtime: shutdown only dropped the retry task's `JoinHandle`, the
    /// parked attempt survived (count 1) and could append the child's
    /// `WorkflowStarted` into a store a successor engine had already swept.
    #[tokio::test(flavor = "multi_thread")]
    async fn epoch_close_drops_the_in_flight_start_attempt() -> TestResult {
        let parent = WorkflowId::new_v4();
        let child = WorkflowId::new_v4();
        let parked = Arc::new(AtomicU32::new(0));
        let gated = Arc::new(GatedParentReadStore {
            inner: InMemoryStore::default(),
            gated_workflow_id: parent.clone(),
            gate: tokio::sync::Notify::new(),
            parked: Arc::clone(&parked),
        });
        let store: Arc<dyn EventStore> = Arc::clone(&gated) as Arc<dyn EventStore>;
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let bridge = Arc::new(ChildNifBridge::new(ChildNifBridgeParts {
            store,
            visibility_store: Arc::new(InMemoryStore::default()),
            runtime: Arc::clone(&runtime),
            catalog: Arc::new(WorkflowCatalog::new()),
            registry: Arc::new(Registry::default()),
            supervision: Arc::new(SupervisionTree::new()),
            signal_handoff: Arc::new(SignalResumeHandoff::new()),
            search_attribute_schema: Arc::new(aion_core::SearchAttributeSchema::new()),
            tokio_handle: tokio::runtime::Handle::current(),
            watch_backoff: fast_backoff(),
        })?);

        assert!(ensure_child_started_in_background(
            &bridge,
            RecordedChildSpawn {
                parent_workflow_id: parent.clone(),
                child_workflow_id: child.clone(),
                workflow_type: "never_loaded_child".to_owned(),
                input: Payload::new(ContentType::Json, br#""input""#.to_vec()),
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
                namespace: String::from("default"),
            },
        ));

        // The attempt reaches the gated parent read mid-flight.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while parked.load(Ordering::Acquire) == 0 {
            if std::time::Instant::now() > deadline {
                return Err("the start attempt never reached the parent read".into());
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        // Epoch close: abort AND await — the parked attempt future must be
        // dropped before shutdown returns.
        bridge.shutdown_child_tasks();
        assert_eq!(
            parked.load(Ordering::Acquire),
            0,
            "the in-flight start attempt must be dropped by the epoch close (N-4)"
        );

        // A zombie would resume here and march toward the append; a dropped
        // future cannot. The child's history stays empty.
        gated.gate.notify_waiters();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            aion_store::ReadableEventStore::read_history(&gated.inner, &child)
                .await?
                .is_empty()
        );
        assert!(
            aion_store::ReadableEventStore::read_history(&gated.inner, &parent)
                .await?
                .is_empty()
        );
        runtime.shutdown()?;
        Ok(())
    }

    /// F3: a post-record start failure is an engine-internal obligation —
    /// arming succeeds, no error escapes toward workflow code, and the retry
    /// keeps the obligation alive until the epoch ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn permanent_start_failure_stays_internal_and_armed() -> TestResult {
        let (bridge, runtime) = bridge_without_loaded_workflows()?;
        let child = WorkflowId::new_v4();

        assert!(ensure_child_started_in_background(
            &bridge,
            recorded_spawn(&child)
        ));
        // Re-arming for the same recorded child is a no-op.
        assert!(!ensure_child_started_in_background(
            &bridge,
            recorded_spawn(&child)
        ));
        assert_eq!(bridge.child_tasks().armed_spawn_retry_count(), 1);

        // The type is never loadable, so the retry keeps failing internally:
        // nothing is started and nothing panics or surfaces.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(bridge.store().read_history(&child).await?.is_empty());
        assert_eq!(bridge.child_tasks().armed_spawn_retry_count(), 1);

        // Epoch close aborts and awaits the obligation.
        bridge.shutdown_child_tasks();
        assert_eq!(bridge.child_tasks().armed_spawn_retry_count(), 0);
        runtime.shutdown()?;
        Ok(())
    }

    /// Store truth governs idempotence: a child whose history already exists
    /// is never started again.
    #[tokio::test(flavor = "multi_thread")]
    async fn existing_child_history_ends_the_retry_without_a_second_start() -> TestResult {
        let (bridge, runtime) = bridge_without_loaded_workflows()?;
        let child = WorkflowId::new_v4();
        let event = aion_core::Event::WorkflowStarted {
            envelope: aion_core::EventEnvelope {
                seq: 1,
                recorded_at: chrono::Utc::now(),
                workflow_id: child.clone(),
            },
            workflow_type: "never_loaded_child".to_owned(),
            input: Payload::new(ContentType::Json, br#""input""#.to_vec()),
            run_id: aion_core::RunId::new_v4(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        };
        bridge
            .store()
            .append(aion_store::WriteToken::recorder(), &child, &[event], 0)
            .await?;

        assert!(ensure_child_started_in_background(
            &bridge,
            recorded_spawn(&child)
        ));

        // The task observes the existing history and finishes without ever
        // attempting a start (the type is not loadable, so an attempted
        // start could never have produced this single-event history).
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while bridge.child_tasks().armed_spawn_retry_count() != 0 {
            if std::time::Instant::now() > deadline {
                return Err("spawn retry did not finish against existing history".into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(bridge.store().read_history(&child).await?.len(), 1);
        bridge.shutdown_child_tasks();
        runtime.shutdown()?;
        Ok(())
    }
}
