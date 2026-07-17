use std::sync::Arc;

use aion_core::{
    ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload, RunId, WorkflowId,
    WorkflowStatus,
};
use aion_package::ContentHash;

use crate::error::EngineError;
use crate::registry::Registry;
use crate::registry::handle::{
    CompletionNotifier, HandleResidency, WorkflowHandle, WorkflowHandleParts,
};
use crate::runtime::config::RuntimeConfig;

use super::RuntimeHandle;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn live_handle(workflow_id: &WorkflowId, run_id: &RunId, pid: u64) -> WorkflowHandle {
    let store = Arc::new(aion_store::InMemoryStore::default());
    let recorder = crate::durability::Recorder::new(workflow_id.clone(), store);
    WorkflowHandle::new(WorkflowHandleParts {
        workflow_id: workflow_id.clone(),
        run_id: run_id.clone(),
        pid,
        workflow_type: "checkout".to_owned(),
        namespace: String::from("default"),
        loaded_version: ContentHash::from_bytes([1; 32]),
        cached_status: WorkflowStatus::Running,
        residency: HandleResidency::Resident,
        recorder,
        completion: CompletionNotifier::new(),
    })
}

fn deliver_after_death_cleanup<F, R>(
    runtime: &Arc<RuntimeHandle>,
    workflow_pid: u64,
    deliver: F,
    retained: R,
) -> TestResult
where
    F: FnOnce(Arc<RuntimeHandle>, u64) -> Result<(), EngineError> + Send + 'static,
    R: FnOnce(&RuntimeHandle, u64) -> bool,
{
    // Model the reviewed beamr ordering: the exit tombstone woke the
    // monitor, but process-table removal is deliberately withheld.
    runtime.drain_activity_completions(workflow_pid)?;
    assert!(
        runtime.is_live(workflow_pid),
        "the regression requires a dead-marked pid that remains in the process table"
    );

    let delivery_result = deliver(Arc::clone(runtime), workflow_pid);
    assert!(
        delivery_result.is_err(),
        "delivery starting behind death cleanup must observe the dead workflow"
    );
    assert!(
        !retained(runtime, workflow_pid),
        "delivery starting behind death cleanup must not retain an outcome"
    );
    runtime.cancel_pid(workflow_pid)?;
    Ok(())
}

#[test]
fn outbox_completion_lands_where_take_reads_it() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let registry = Registry::default();
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    // A live test process supplies the pid the registry resolves to.
    let pid = runtime.spawn_test_process()?;
    registry.insert(
        (workflow_id.clone(), run_id.clone()),
        live_handle(&workflow_id, &run_id, pid),
    )?;

    let ordinal = 3;
    let activity_id = ActivityId::from_sequence_position(ordinal);
    let delivered = runtime.deliver_outbox_completion(
        &registry,
        &workflow_id,
        &activity_id,
        None,
        r#"{"ok":true}"#.to_owned(),
    )?;

    assert!(delivered, "delivery to a live workflow must report true");
    let payload = runtime
        .take_activity_result(pid, ordinal)
        .ok_or("completion was not retained where take_activity_result reads it")?;
    assert_eq!(payload.bytes(), br#"{"ok":true}"#);

    // An unknown workflow id is the not-live outcome, never an error.
    let unknown = runtime.deliver_outbox_completion(
        &registry,
        &WorkflowId::new_v4(),
        &activity_id,
        None,
        "{}".to_owned(),
    )?;
    assert!(
        !unknown,
        "an unknown workflow must report not-live, not error"
    );

    runtime.shutdown()?;
    Ok(())
}

#[test]
fn outbox_completion_is_run_scoped_across_continue_as_new() -> Result<(), Box<dyn std::error::Error>>
{
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let registry = Registry::default();
    let workflow_id = WorkflowId::new_v4();
    // R1 is the prior run; R2 is the live run after a continue-as-new. The
    // index tracks the newest run, so the workflow's live run is R2.
    let r1 = RunId::new_v4();
    let r2 = RunId::new_v4();
    let pid = runtime.spawn_test_process()?;
    registry.insert(
        (workflow_id.clone(), r2.clone()),
        live_handle(&workflow_id, &r2, pid),
    )?;

    // A reused ordinal that exists in both R1's and R2's ordinal space.
    let ordinal = 3;
    let activity_id = ActivityId::from_sequence_position(ordinal);

    // A completion belonging to the superseded run R1 must NOT be delivered
    // and must NOT resolve R2's reused ordinal.
    let stale = runtime.deliver_outbox_completion(
        &registry,
        &workflow_id,
        &activity_id,
        Some(&r1),
        r#"{"from":"r1"}"#.to_owned(),
    )?;
    assert!(
        !stale,
        "a completion for a superseded run must not be delivered"
    );
    assert!(
        runtime.take_activity_result(pid, ordinal).is_none(),
        "a superseded run's completion must not resolve the live run's reused ordinal"
    );

    // A completion for the live run R2 IS delivered and resolves the ordinal.
    let live = runtime.deliver_outbox_completion(
        &registry,
        &workflow_id,
        &activity_id,
        Some(&r2),
        r#"{"from":"r2"}"#.to_owned(),
    )?;
    assert!(live, "a completion for the live run must be delivered");
    let payload = runtime
        .take_activity_result(pid, ordinal)
        .ok_or("live-run completion was not retained where take_activity_result reads it")?;
    assert_eq!(payload.bytes(), br#"{"from":"r2"}"#);

    runtime.shutdown()?;
    Ok(())
}

#[test]
fn outbox_failure_is_run_scoped_across_continue_as_new() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let registry = Registry::default();
    let workflow_id = WorkflowId::new_v4();
    let r1 = RunId::new_v4();
    let r2 = RunId::new_v4();
    let pid = runtime.spawn_test_process()?;
    registry.insert(
        (workflow_id.clone(), r2.clone()),
        live_handle(&workflow_id, &r2, pid),
    )?;

    let ordinal = 5;
    let activity_id = ActivityId::from_sequence_position(ordinal);

    let stale = runtime.deliver_outbox_failure(
        &registry,
        &workflow_id,
        &activity_id,
        Some(&r1),
        "r1 failed".to_owned(),
    )?;
    assert!(
        !stale,
        "a failure for a superseded run must not be delivered"
    );
    assert!(
        runtime.take_activity_error(pid, ordinal).is_none(),
        "a superseded run's failure must not resolve the live run's reused ordinal"
    );

    let live = runtime.deliver_outbox_failure(
        &registry,
        &workflow_id,
        &activity_id,
        Some(&r2),
        "r2 failed".to_owned(),
    )?;
    assert!(live, "a failure for the live run must be delivered");
    assert!(
        runtime.take_activity_error(pid, ordinal).is_some(),
        "live-run failure must be retained where take_activity_error reads it"
    );

    runtime.shutdown()?;
    Ok(())
}

#[test]
fn death_cleanup_blocks_all_production_activity_retention_paths() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);

    let completion_pid = runtime.spawn_test_process()?;
    deliver_after_death_cleanup(
        &runtime,
        completion_pid,
        |runtime, pid| {
            runtime.deliver_activity_completion_message(
                pid,
                "activity:7",
                r#"{"ok":true}"#.to_owned(),
            )
        },
        |runtime, pid| runtime.activity_result(pid, 7).is_some(),
    )?;
    deliver_after_death_cleanup(
        &runtime,
        runtime.spawn_test_process()?,
        |runtime, pid| {
            runtime.deliver_activity_failure_message(pid, "activity:9", "failed".to_owned())
        },
        |runtime, pid| runtime.activity_error(pid, 9).is_some(),
    )?;

    deliver_after_death_cleanup(
        &runtime,
        runtime.spawn_test_process()?,
        |runtime, pid| {
            runtime.deliver_activity_result(
                pid,
                11,
                Payload::new(ContentType::Json, br#"{"legacy":true}"#.to_vec()),
            )
        },
        |runtime, pid| runtime.activity_result(pid, 11).is_some(),
    )?;

    deliver_after_death_cleanup(
        &runtime,
        runtime.spawn_test_process()?,
        |runtime, pid| {
            runtime.deliver_activity_error(
                pid,
                13,
                ActivityError {
                    kind: ActivityErrorKind::Terminal,
                    message: "legacy failure".to_owned(),
                    details: None,
                },
            )
        },
        |runtime, pid| runtime.activity_error(pid, 13).is_some(),
    )?;

    runtime.shutdown()?;
    Ok(())
}

#[test]
fn workflow_death_between_retention_and_enqueue_rolls_back_outcomes() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;

    let completion_pid = runtime.spawn_test_process()?;
    let completion_key = (completion_pid, 7);
    runtime.note_delivery_attempt(completion_pid, 7, 3);
    let completion = runtime.retain_activity_outcome_until_marker_delivery(
        completion_pid,
        &runtime.activity_results,
        completion_key,
        Payload::new(ContentType::Json, br#"{"ok":true}"#.to_vec()),
        || {
            runtime.cancel_pid(completion_pid)?;
            runtime.enqueue_activity_marker(
                completion_pid,
                runtime.activity_complete_atom(),
                "activity:7",
            )
        },
    );
    assert!(
        completion.is_err(),
        "completion marker enqueue after workflow death must fail"
    );
    assert!(
        runtime.activity_result(completion_pid, 7).is_none(),
        "failed completion delivery must remove its retained payload"
    );
    assert!(
        runtime.take_delivery_attempt(completion_pid, 7).is_none(),
        "failed completion delivery must remove its retained attempt"
    );

    let failure_pid = runtime.spawn_test_process()?;
    let failure_key = (failure_pid, 9);
    runtime.note_delivery_attempt(failure_pid, 9, 4);
    let failure = runtime.retain_activity_outcome_until_marker_delivery(
        failure_pid,
        &runtime.activity_errors,
        failure_key,
        ActivityError {
            kind: ActivityErrorKind::Terminal,
            message: "failed".to_owned(),
            details: None,
        },
        || {
            runtime.cancel_pid(failure_pid)?;
            runtime.enqueue_activity_marker(
                failure_pid,
                runtime.activity_failed_atom(),
                "activity:9",
            )
        },
    );
    assert!(
        failure.is_err(),
        "failure marker enqueue after workflow death must fail"
    );
    assert!(
        runtime.activity_error(failure_pid, 9).is_none(),
        "failed failure delivery must remove its retained error"
    );
    assert!(
        runtime.take_delivery_attempt(failure_pid, 9).is_none(),
        "failed failure delivery must remove its retained attempt"
    );

    runtime.shutdown()?;
    Ok(())
}

#[test]
fn stalled_workflow_cleanup_does_not_block_unrelated_delivery() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let stalled_pid = runtime.spawn_test_process()?;
    let unrelated_pid = runtime.spawn_test_process()?;

    // Model an observed exit tombstone while deliberately withholding
    // process-table removal. Cleanup must return with this pid still live.
    runtime.drain_activity_completions(stalled_pid)?;
    assert!(
        runtime.is_live(stalled_pid),
        "the stalled-removal regression requires the dead pid to remain in the process table"
    );

    runtime.deliver_activity_completion_message(
        unrelated_pid,
        "activity:17",
        r#"{"unrelated":true}"#.to_owned(),
    )?;
    assert!(
        runtime.activity_result(unrelated_pid, 17).is_some(),
        "a dead-marked workflow must not block an unrelated workflow's delivery"
    );

    runtime.cancel_pid(stalled_pid)?;
    runtime.shutdown()?;
    Ok(())
}
