use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aion_core::{
    ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload, RunId, WorkflowId,
};

use crate::error::EngineError;
use crate::registry::Registry;
use crate::runtime::config::RuntimeConfig;
use crate::runtime::handle::RuntimeHandle;
use crate::runtime::handle::activity_delivery::{ActivityOutcomeKind, RetainedActivityDelivery};

use super::{TestResult, live_handle};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_POLL_INTERVAL: Duration = Duration::from_millis(1);

fn test_runtime_error(reason: impl Into<String>) -> EngineError {
    EngineError::Runtime {
        reason: reason.into(),
    }
}

fn join_test_thread<T>(thread: JoinHandle<T>) -> Result<T, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(TEST_POLL_INTERVAL);
    }
    if !thread.is_finished() {
        return Err("activity-delivery test worker timed out".into());
    }
    Ok(thread
        .join()
        .map_err(|_| "activity-delivery test worker panicked")?)
}

fn wait_until(mut condition: impl FnMut() -> bool, timeout_message: &'static str) -> TestResult {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        if condition() {
            return Ok(());
        }
        std::thread::sleep(TEST_POLL_INTERVAL);
    }
    Err(timeout_message.into())
}

#[test]
fn monitor_drain_overlaps_completion_before_enqueue_and_rolls_it_back() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let baseline = runtime.activity_delivery_gate_count();
    let workflow_pid = runtime.spawn_test_process()?;
    let (monitor_sender, monitor_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(workflow_pid, move |outcome| {
        if monitor_sender.send(outcome).is_err() {
            tracing::error!("completion-overlap monitor receiver dropped");
        }
    })?;

    let (marker_sender, marker_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let delivery_runtime = Arc::clone(&runtime);
    let delivery_thread = std::thread::spawn(move || {
        let key = (workflow_pid, 7);
        delivery_runtime.retain_activity_outcome_and_deliver_marker(
            workflow_pid,
            &delivery_runtime.activity_results,
            RetainedActivityDelivery {
                key,
                outcome: Payload::new(ContentType::Json, br#"{"ok":true}"#.to_vec()),
                kind: ActivityOutcomeKind::Result,
                attempt: Some(3),
            },
            || {
                marker_sender
                    .send(())
                    .map_err(|error| test_runtime_error(error.to_string()))?;
                release_receiver
                    .recv_timeout(TEST_TIMEOUT)
                    .map_err(|error| test_runtime_error(error.to_string()))?;
                delivery_runtime.enqueue_activity_marker(
                    workflow_pid,
                    delivery_runtime.activity_complete_atom(),
                    7,
                    "activity:7",
                )
            },
        )
    });

    marker_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(runtime.activity_result(workflow_pid, 7).is_some());
    runtime.cancel_pid(workflow_pid)?;
    wait_until(
        || runtime.activity_delivery_cleanup_started_for_test(workflow_pid),
        "monitor did not enter completion cleanup",
    )?;
    release_sender.send(())?;

    let delivery = join_test_thread(delivery_thread)?;
    assert!(delivery.is_err(), "enqueue after observed death must fail");
    let monitor_outcome = monitor_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(
        monitor_outcome.is_ok(),
        "clean drain must preserve the process outcome"
    );
    assert!(runtime.activity_result(workflow_pid, 7).is_none());
    assert!(
        runtime
            .activity_delivery_attempts
            .get(&(workflow_pid, 7))
            .is_none()
    );
    assert_eq!(runtime.activity_delivery_gate_count(), baseline);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn monitor_drain_overlaps_failure_before_enqueue_and_rolls_it_back() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let baseline = runtime.activity_delivery_gate_count();
    let workflow_pid = runtime.spawn_test_process()?;
    let (monitor_sender, monitor_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(workflow_pid, move |outcome| {
        if monitor_sender.send(outcome).is_err() {
            tracing::error!("failure-overlap monitor receiver dropped");
        }
    })?;

    let (marker_sender, marker_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let delivery_runtime = Arc::clone(&runtime);
    let delivery_thread = std::thread::spawn(move || {
        let key = (workflow_pid, 9);
        delivery_runtime.retain_activity_outcome_and_deliver_marker(
            workflow_pid,
            &delivery_runtime.activity_errors,
            RetainedActivityDelivery {
                key,
                outcome: ActivityError {
                    kind: ActivityErrorKind::Terminal,
                    message: "failed".to_owned(),
                    details: None,
                },
                kind: ActivityOutcomeKind::Error,
                attempt: Some(4),
            },
            || {
                marker_sender
                    .send(())
                    .map_err(|error| test_runtime_error(error.to_string()))?;
                release_receiver
                    .recv_timeout(TEST_TIMEOUT)
                    .map_err(|error| test_runtime_error(error.to_string()))?;
                delivery_runtime.enqueue_activity_marker(
                    workflow_pid,
                    delivery_runtime.activity_failed_atom(),
                    9,
                    "activity:9",
                )
            },
        )
    });

    marker_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(runtime.activity_error(workflow_pid, 9).is_some());
    runtime.cancel_pid(workflow_pid)?;
    wait_until(
        || runtime.activity_delivery_cleanup_started_for_test(workflow_pid),
        "monitor did not enter failure cleanup",
    )?;
    release_sender.send(())?;

    let delivery = join_test_thread(delivery_thread)?;
    assert!(delivery.is_err(), "failure enqueue after death must fail");
    let monitor_outcome = monitor_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(
        monitor_outcome.is_ok(),
        "clean drain must preserve the process outcome"
    );
    assert!(runtime.activity_error(workflow_pid, 9).is_none());
    assert!(
        runtime
            .activity_delivery_attempts
            .get(&(workflow_pid, 9))
            .is_none()
    );
    assert_eq!(runtime.activity_delivery_gate_count(), baseline);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn monitor_drains_outcome_when_death_follows_successful_enqueue() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let baseline = runtime.activity_delivery_gate_count();
    let workflow_pid = runtime.spawn_test_process()?;
    let (monitor_sender, monitor_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(workflow_pid, move |outcome| {
        if monitor_sender.send(outcome).is_err() {
            tracing::error!("post-enqueue monitor receiver dropped");
        }
    })?;

    let (enqueued_sender, enqueued_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let delivery_runtime = Arc::clone(&runtime);
    let delivery_thread = std::thread::spawn(move || {
        let key = (workflow_pid, 11);
        delivery_runtime.retain_activity_outcome_and_deliver_marker(
            workflow_pid,
            &delivery_runtime.activity_results,
            RetainedActivityDelivery {
                key,
                outcome: Payload::new(ContentType::Json, br#"{"ok":true}"#.to_vec()),
                kind: ActivityOutcomeKind::Result,
                attempt: None,
            },
            || {
                delivery_runtime.enqueue_activity_marker(
                    workflow_pid,
                    delivery_runtime.activity_complete_atom(),
                    11,
                    "activity:11",
                )?;
                enqueued_sender
                    .send(())
                    .map_err(|error| test_runtime_error(error.to_string()))?;
                release_receiver
                    .recv_timeout(TEST_TIMEOUT)
                    .map_err(|error| test_runtime_error(error.to_string()))?;
                Ok(())
            },
        )
    });

    enqueued_receiver.recv_timeout(TEST_TIMEOUT)?;
    runtime.cancel_pid(workflow_pid)?;
    wait_until(
        || runtime.activity_delivery_cleanup_started_for_test(workflow_pid),
        "monitor did not contend with post-enqueue delivery",
    )?;
    release_sender.send(())?;

    join_test_thread(delivery_thread)??;
    let monitor_outcome = monitor_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(
        monitor_outcome.is_ok(),
        "clean drain must preserve the process outcome"
    );
    assert!(runtime.activity_result(workflow_pid, 11).is_none());
    assert_eq!(runtime.activity_delivery_gate_count(), baseline);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn poisoned_monitor_drain_removes_state_and_returns_typed_error() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let baseline = runtime.activity_delivery_gate_count();
    let workflow_pid = runtime.spawn_test_process()?;
    runtime.deliver_activity_completion_message_with_attempt(
        workflow_pid,
        "activity:13",
        r#"{"retained":true}"#.to_owned(),
        Some(2),
    )?;
    runtime.force_activity_delivery_poison_for_test(workflow_pid)?;

    let ordinary = runtime.deliver_activity_failure_message(
        workflow_pid,
        "activity:15",
        "must fail closed".to_owned(),
    );
    assert!(matches!(
        ordinary,
        Err(EngineError::ActivityDeliveryPoisoned { process_id }) if process_id == workflow_pid
    ));
    assert!(runtime.activity_error(workflow_pid, 15).is_none());

    let (monitor_sender, monitor_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(workflow_pid, move |outcome| {
        if monitor_sender.send(outcome).is_err() {
            tracing::error!("poison monitor receiver dropped");
        }
    })?;
    runtime.cancel_pid(workflow_pid)?;
    let monitored = monitor_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(matches!(
        monitored,
        Err(EngineError::ActivityDeliveryPoisoned { process_id }) if process_id == workflow_pid
    ));
    assert!(runtime.activity_result(workflow_pid, 13).is_none());
    assert!(
        runtime
            .activity_delivery_attempts
            .get(&(workflow_pid, 13))
            .is_none(),
        "poisoned destructive cleanup must remove attempt metadata"
    );
    assert_eq!(runtime.activity_delivery_gate_count(), baseline);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn withheld_removal_does_not_block_reregistered_workflow_delivery() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let registry = Arc::new(Registry::default());
    let workflow_id = WorkflowId::new_v4();
    let old_run = RunId::new_v4();
    let new_run = RunId::new_v4();
    let old_pid = runtime.spawn_test_process()?;
    let new_pid = runtime.spawn_test_process()?;

    runtime.deliver_activity_completion_message(
        new_pid,
        "activity:1",
        r#"{"seed":true}"#.to_owned(),
    )?;
    let seed = runtime
        .take_activity_result(new_pid, 1)?
        .ok_or("new workflow gate was not seeded")?;
    drop(seed);
    let baseline = runtime.activity_delivery_gate_count();
    runtime.deliver_activity_completion_message(
        old_pid,
        "activity:17",
        r#"{"old":true}"#.to_owned(),
    )?;
    registry.insert(
        (workflow_id.clone(), old_run.clone()),
        live_handle(&workflow_id, &old_run, old_pid),
    )?;

    let (monitor_sender, monitor_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(old_pid, move |outcome| {
        if monitor_sender.send(outcome).is_err() {
            tracing::error!("withheld-removal monitor receiver dropped");
        }
    })?;
    let held_process = runtime
        .scheduler
        .process_table()
        .get(old_pid)
        .ok_or("old workflow was absent before termination")?;
    let cancel_runtime = Arc::clone(&runtime);
    let cancel_thread = std::thread::spawn(move || cancel_runtime.cancel_pid(old_pid));

    wait_until(
        || runtime.scheduler.peek_exit_reason(old_pid).is_some(),
        "old workflow never published an exit tombstone",
    )?;
    assert!(
        runtime.is_live(old_pid),
        "held process-table reference must withhold removal"
    );
    wait_until(
        || runtime.activity_result(old_pid, 17).is_none(),
        "monitor did not drain the old workflow while removal was withheld",
    )?;

    registry.insert(
        (workflow_id.clone(), new_run.clone()),
        live_handle(&workflow_id, &new_run, new_pid),
    )?;
    let (delivery_sender, delivery_receiver) = mpsc::channel();
    let delivery_runtime = Arc::clone(&runtime);
    let delivery_registry = Arc::clone(&registry);
    let delivery_workflow_id = workflow_id.clone();
    let delivery_thread = std::thread::spawn(move || {
        let activity_id = ActivityId::from_sequence_position(19);
        let result = delivery_runtime.deliver_outbox_completion(
            &delivery_registry,
            &delivery_workflow_id,
            &activity_id,
            Some(&new_run),
            r#"{"new":true}"#.to_owned(),
        );
        delivery_sender
            .send(matches!(&result, Ok(true)))
            .map_err(|error| test_runtime_error(error.to_string()))?;
        result
    });

    let delivered_before_release = delivery_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(
        delivered_before_release,
        "new run delivery must finish before old process removal is released"
    );
    drop(held_process);
    join_test_thread(cancel_thread)??;
    assert!(join_test_thread(delivery_thread)??);
    let monitor_outcome = monitor_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(
        monitor_outcome.is_ok(),
        "old workflow cleanup must complete"
    );

    let (payload, attempt) = runtime
        .take_activity_result(new_pid, 19)?
        .ok_or("re-registered workflow did not retain its completion")?;
    assert_eq!(payload.bytes(), br#"{"new":true}"#);
    assert_eq!(attempt, None);
    assert!(runtime.activity_result(old_pid, 17).is_none());
    assert_eq!(runtime.activity_delivery_gate_count(), baseline);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn late_delivery_after_monitor_cleanup_does_not_recreate_a_gate() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let baseline = runtime.activity_delivery_gate_count();
    let workflow_pid = runtime.spawn_test_process()?;
    let (monitor_sender, monitor_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(workflow_pid, move |outcome| {
        if monitor_sender.send(outcome).is_err() {
            tracing::error!("late-delivery monitor receiver dropped");
        }
    })?;

    runtime.cancel_pid(workflow_pid)?;
    let monitor_outcome = monitor_receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(monitor_outcome.is_ok(), "monitor cleanup must finish");
    assert_eq!(runtime.activity_delivery_gate_count(), baseline);

    let completion = runtime.deliver_activity_completion_message(
        workflow_pid,
        "activity:21",
        r#"{"late":true}"#.to_owned(),
    );
    let failure = runtime.deliver_activity_failure_message(
        workflow_pid,
        "activity:22",
        "late failure".to_owned(),
    );
    let legacy_result = runtime.deliver_activity_result(
        workflow_pid,
        23,
        Payload::new(ContentType::Json, br#"{"late":true}"#.to_vec()),
    );
    let legacy_error = runtime.deliver_activity_error(
        workflow_pid,
        24,
        ActivityError {
            kind: ActivityErrorKind::Terminal,
            message: "late legacy failure".to_owned(),
            details: None,
        },
    );
    let attempted_completion = runtime.deliver_activity_completion_message_with_attempt(
        workflow_pid,
        "activity:25",
        r#"{"late":true}"#.to_owned(),
        Some(3),
    );

    assert!(completion.is_err());
    assert!(failure.is_err());
    assert!(legacy_result.is_err());
    assert!(legacy_error.is_err());
    assert!(attempted_completion.is_err());
    assert_eq!(runtime.retained_activity_completions(), 0);
    assert!(
        runtime
            .activity_delivery_attempts
            .get(&(workflow_pid, 25))
            .is_none()
    );
    assert_eq!(runtime.activity_delivery_gate_count(), baseline);
    runtime.shutdown()?;
    Ok(())
}
