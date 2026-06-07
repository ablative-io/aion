//! End-to-end schedule lifecycle integration tests over `InMemoryStore`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aion::durability::{ActiveWorkflowRecovery, ActiveWorkflowRecoverySeam};
use aion::schedule::TimerEvaluationOutcome;
use aion::{Engine, EngineBuilder, EngineError, LoadedWorkflows, Pid, WorkflowHandle};
use aion_core::{
    CatchUpPolicy, Event, OverlapPolicy, RunId, ScheduleConfig, TriggerSpec, WorkflowId,
};
use aion_package::ContentHash;
use aion_store::EventStore;
use serde_json::json;

#[tokio::test]
async fn schedule_lifecycle_fires_skips_overlap_and_recovers()
-> Result<(), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(aion_store::InMemoryStore::default());
    let recovery = Arc::new(TestRecovery::default());
    let engine = engine_with_store(Arc::clone(&store), recovery.clone()).await?;
    let input = common::payload(&json!({ "scheduled": true }))?;
    let config = ScheduleConfig {
        trigger: TriggerSpec::Interval {
            period: Duration::from_millis(25),
        },
        overlap_policy: OverlapPolicy::Skip,
        catch_up_policy: CatchUpPolicy::Skip,
        workflow_type: common::FIXTURE_MODULE.to_owned(),
        input: input.clone(),
    };

    let schedule_id = engine.create_schedule(config.clone()).await?;
    let created_state = engine.describe_schedule(&schedule_id).await?;
    let first_fire_at = created_state.next_trigger_at;

    let first = engine
        .handle_schedule_timer_fired(&schedule_id, first_fire_at)
        .await?;
    let TimerEvaluationOutcome::Started(first_execution) = first else {
        return Err("expected first schedule timer to start workflow".into());
    };
    let first_history = store.read_history(&first_execution.workflow_id).await?;
    assert!(first_history.iter().any(|event| matches!(
        event,
        Event::WorkflowStarted { input, .. } if input == &config.input
    )));
    assert_schedule_triggered(&engine, &schedule_id, &first_execution.workflow_id).await?;

    let first_handle = engine
        .registry()
        .get(&first_execution.workflow_id, &first_execution.run_id)?
        .ok_or_else(|| EngineError::Load {
            reason: format!(
                "test recovery could not find live handle for workflow {}",
                first_execution.workflow_id
            ),
        })?;
    recovery.record(&first_handle)?;

    let state_after_first = engine.describe_schedule(&schedule_id).await?;
    let skipped = engine
        .handle_schedule_timer_fired(&schedule_id, state_after_first.next_trigger_at)
        .await?;
    assert_eq!(skipped, TimerEvaluationOutcome::Skipped);
    let after_skip_triggers = schedule_trigger_count(&engine, &schedule_id).await?;
    assert_eq!(after_skip_triggers, 1);

    engine.shutdown()?;
    let recovered = engine_with_store(Arc::clone(&store), recovery).await?;

    let recovered_schedules = recovered.list_schedules().await?;
    let [listed_state] = recovered_schedules.as_slice() else {
        return Err(format!(
            "expected exactly one recovered schedule, found {}",
            recovered_schedules.len()
        )
        .into());
    };
    assert_eq!(listed_state.schedule_id, schedule_id);
    assert_eq!(listed_state.config, config);
    assert!(!listed_state.is_paused);
    assert!(!listed_state.is_deleted);
    assert!(listed_state.current_execution.is_none());
    assert!(listed_state.last_triggered_at.is_some());

    let recovered_state = recovered.describe_schedule(&schedule_id).await?;
    let recovered_fire = recovered
        .handle_schedule_timer_fired(&schedule_id, recovered_state.next_trigger_at)
        .await?;
    let TimerEvaluationOutcome::Started(_) = recovered_fire else {
        return Err("expected recovered schedule to re-arm and fire again".into());
    };
    let after_recovery_triggers = schedule_trigger_count(&recovered, &schedule_id).await?;
    assert_eq!(after_recovery_triggers, 2);

    Ok(())
}

#[tokio::test]
async fn schedule_uses_common_helpers_for_engine_and_payload()
-> Result<(), Box<dyn std::error::Error>> {
    let (engine, _store) = common::engine_with_fixture("wait").await?;
    let input = common::input_payload()?;
    let config = ScheduleConfig {
        trigger: TriggerSpec::Interval {
            period: Duration::from_secs(3600),
        },
        overlap_policy: OverlapPolicy::Skip,
        catch_up_policy: CatchUpPolicy::Skip,
        workflow_type: common::FIXTURE_MODULE.to_owned(),
        input,
    };
    let schedule_id = engine.create_schedule(config).await?;
    let state = engine.describe_schedule(&schedule_id).await?;
    assert!(!state.is_paused);
    Ok(())
}

async fn engine_with_store(
    store: Arc<dyn EventStore>,
    recovery: Arc<dyn ActiveWorkflowRecoverySeam>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(store)
        .scheduler_threads(1)
        .recovery_seam(recovery)
        .load_workflows(common::fixture_package("wait")?)
        .build()
        .await?)
}

async fn assert_schedule_triggered(
    engine: &Engine,
    schedule_id: &aion_core::ScheduleId,
    workflow_id: &aion_core::WorkflowId,
) -> Result<(), Box<dyn std::error::Error>> {
    let history = engine
        .store()
        .read_history(engine.schedule_coordinator_workflow_id())
        .await?;
    assert!(history.iter().any(|event| matches!(
        event,
        Event::ScheduleTriggered { schedule_id: recorded_schedule_id, workflow_id: recorded_workflow_id, .. }
            if recorded_schedule_id == schedule_id && recorded_workflow_id == workflow_id
    )));
    Ok(())
}

async fn schedule_trigger_count(
    engine: &Engine,
    schedule_id: &aion_core::ScheduleId,
) -> Result<usize, Box<dyn std::error::Error>> {
    let history = engine
        .store()
        .read_history(engine.schedule_coordinator_workflow_id())
        .await?;
    Ok(history
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::ScheduleTriggered { schedule_id: recorded_schedule_id, .. }
                    if recorded_schedule_id == schedule_id
            )
        })
        .count())
}

#[derive(Debug, Default)]
struct TestRecovery {
    replacements: Mutex<Vec<RecoveryEntry>>,
}

impl TestRecovery {
    fn record(&self, handle: &WorkflowHandle) -> Result<(), EngineError> {
        self.replacements
            .lock()
            .map_err(|_| EngineError::RegistryPoisoned)?
            .push(RecoveryEntry {
                workflow_id: handle.workflow_id().clone(),
                run_id: handle.run_id().clone(),
                version: handle.loaded_version().clone(),
                pid: handle.pid(),
            });
        Ok(())
    }
}

#[derive(Debug)]
struct RecoveryEntry {
    workflow_id: WorkflowId,
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
        let _ = (workflow_type, history, loaded_workflows);
        let replacements = self
            .replacements
            .lock()
            .map_err(|_| EngineError::RegistryPoisoned)?;
        let entry = replacements
            .iter()
            .find(|entry| &entry.workflow_id == workflow_id)
            .ok_or_else(|| EngineError::Load {
                reason: format!("test recovery has no run metadata for workflow {workflow_id}"),
            })?;

        Ok(ActiveWorkflowRecovery::Resident {
            run_id: entry.run_id.clone(),
            loaded_version: entry.version.clone(),
            pid: entry.pid,
        })
    }
}
