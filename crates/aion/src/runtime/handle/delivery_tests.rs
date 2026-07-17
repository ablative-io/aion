use std::sync::Arc;

use aion_core::{ActivityId, RunId, WorkflowId, WorkflowStatus};
use aion_package::ContentHash;

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

#[test]
fn outbox_completion_lands_where_take_reads_it() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let registry = Registry::default();
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
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

    let unknown = runtime.deliver_outbox_completion(
        &registry,
        &WorkflowId::new_v4(),
        &activity_id,
        None,
        "{}".to_owned(),
    )?;
    assert!(!unknown, "an unknown workflow must report not-live");

    runtime.shutdown()?;
    Ok(())
}

#[test]
fn outbox_completion_is_run_scoped_across_continue_as_new() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let registry = Registry::default();
    let workflow_id = WorkflowId::new_v4();
    let prior_run = RunId::new_v4();
    let live_run = RunId::new_v4();
    let pid = runtime.spawn_test_process()?;
    registry.insert(
        (workflow_id.clone(), live_run.clone()),
        live_handle(&workflow_id, &live_run, pid),
    )?;

    let ordinal = 3;
    let activity_id = ActivityId::from_sequence_position(ordinal);
    let stale = runtime.deliver_outbox_completion(
        &registry,
        &workflow_id,
        &activity_id,
        Some(&prior_run),
        r#"{"from":"prior"}"#.to_owned(),
    )?;
    assert!(!stale, "a superseded run must not receive a completion");
    assert!(runtime.take_activity_result(pid, ordinal).is_none());

    let delivered = runtime.deliver_outbox_completion(
        &registry,
        &workflow_id,
        &activity_id,
        Some(&live_run),
        r#"{"from":"live"}"#.to_owned(),
    )?;
    assert!(delivered, "the live run must receive its completion");
    let payload = runtime
        .take_activity_result(pid, ordinal)
        .ok_or("live-run completion was not retained")?;
    assert_eq!(payload.bytes(), br#"{"from":"live"}"#);

    runtime.shutdown()?;
    Ok(())
}

#[test]
fn outbox_failure_is_run_scoped_across_continue_as_new() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let registry = Registry::default();
    let workflow_id = WorkflowId::new_v4();
    let prior_run = RunId::new_v4();
    let live_run = RunId::new_v4();
    let pid = runtime.spawn_test_process()?;
    registry.insert(
        (workflow_id.clone(), live_run.clone()),
        live_handle(&workflow_id, &live_run, pid),
    )?;

    let ordinal = 5;
    let activity_id = ActivityId::from_sequence_position(ordinal);
    let stale = runtime.deliver_outbox_failure(
        &registry,
        &workflow_id,
        &activity_id,
        Some(&prior_run),
        "prior failed".to_owned(),
    )?;
    assert!(!stale, "a superseded run must not receive a failure");
    assert!(runtime.take_activity_error(pid, ordinal).is_none());

    let delivered = runtime.deliver_outbox_failure(
        &registry,
        &workflow_id,
        &activity_id,
        Some(&live_run),
        "live failed".to_owned(),
    )?;
    assert!(delivered, "the live run must receive its failure");
    assert!(runtime.take_activity_error(pid, ordinal).is_some());

    runtime.shutdown()?;
    Ok(())
}

#[path = "delivery_interleaving_tests.rs"]
mod interleavings;
