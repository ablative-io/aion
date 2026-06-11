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

use aion_core::{Payload, WorkflowId};

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
/// or by a startup sweep) and the task is done. Start attempts run on the
/// host runtime — the start path installs exit monitors that capture
/// `Handle::current()`, which must outlive this task's own executor.
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

        let attempt_bridge = Arc::clone(bridge);
        let request = ChildWorkflowSpawnRequest {
            parent_workflow_id: spawn.parent_workflow_id.clone(),
            child_workflow_id: spawn.child_workflow_id.clone(),
            workflow_type: spawn.workflow_type.clone(),
            input: spawn.input.clone(),
        };
        let parent_workflow_id = spawn.parent_workflow_id.clone();
        let attempt = bridge.tokio_handle().spawn(async move {
            attempt_bridge
                .start_child_under_recorded_id(&parent_workflow_id, request)
                .await
                .map(|handle| handle.workflow_id().clone())
        });
        match attempt.await {
            Ok(Ok(started_id)) => {
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
            Ok(Err(error)) => {
                tracing::warn!(
                    parent_workflow_id = %spawn.parent_workflow_id,
                    child_workflow_id = %spawn.child_workflow_id,
                    workflow_type = %spawn.workflow_type,
                    error = %error,
                    "recorded child start failed; retrying with backoff"
                );
            }
            Err(join_error) => {
                tracing::warn!(
                    parent_workflow_id = %spawn.parent_workflow_id,
                    child_workflow_id = %spawn.child_workflow_id,
                    error = %join_error,
                    "recorded child start attempt did not complete; retrying with backoff"
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
    use std::time::Duration;

    use aion_core::{ContentType, Payload, WorkflowId};
    use aion_store::{EventStore, InMemoryStore};

    use super::{RecordedChildSpawn, ensure_child_started_in_background};
    use crate::loader::LoadedWorkflows;
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
            loaded_workflows: LoadedWorkflows::new(),
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
        }
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
