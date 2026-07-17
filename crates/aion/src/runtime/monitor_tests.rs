//! Runtime process monitor installation and rollback tests.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aion_core::{ContentType, Payload};

use super::{UnmonitoredProcessAbortError, WorkflowProcessOutcome};
use crate::runtime::{RuntimeConfig, RuntimeHandle, SignalDeliveryConfig};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn monitor_installs_for_process_that_already_exited() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    runtime.cancel_pid(pid)?;
    assert!(
        !runtime.is_live(pid),
        "terminated test process should leave the live table"
    );

    let (sender, receiver) = mpsc::channel();
    let handle = runtime.monitor_process_for_test(pid, move |outcome| {
        let _ = sender.send(outcome.is_ok());
    })?;

    assert!(handle.is_installed());
    let callback_fired = receiver.recv_timeout(Duration::from_secs(10))?;
    let _ = callback_fired;
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn monitor_rejects_pid_never_spawned_by_this_runtime() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);

    let error = runtime
        .monitor_process_for_test(9_999, |_| {})
        .err()
        .ok_or("monitor accepted a pid this runtime never spawned")?;

    assert!(error.to_string().contains("never spawned"));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn monitor_spawn_failure_drains_retained_completion_transaction() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let baseline_gates = runtime.activity_delivery_gate_count();
    let pid = runtime.spawn_test_process()?;
    runtime.deliver_activity_completion_message_with_attempt(
        pid,
        "activity:41",
        String::from(r#"{"completed":true}"#),
        Some(3),
    )?;
    assert_eq!(
        runtime.activity_result(pid, 41),
        Some(Payload::new(
            ContentType::Json,
            br#"{"completed":true}"#.to_vec()
        ))
    );
    assert_eq!(runtime.retained_activity_attempt_count_for_test(), 1);
    assert_eq!(runtime.activity_delivery_gate_count(), baseline_gates + 1);

    runtime.force_next_monitor_spawn_failure_for_test();
    let error = runtime
        .monitor_process_for_test(pid, |_| {})
        .err()
        .ok_or("forced monitor spawn failure installed a monitor")?;

    assert!(
        error.to_string().contains("forced test failure"),
        "typed monitor installation error must remain visible"
    );
    assert!(
        !runtime.is_live(pid),
        "failed monitor installation must synchronously terminate the process"
    );
    assert_eq!(runtime.retained_activity_completions(), 0);
    assert_eq!(runtime.retained_activity_attempt_count_for_test(), 0);
    assert_eq!(runtime.activity_delivery_gate_count(), baseline_gates);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn duplicate_installation_cannot_consume_outcome_or_abort_owned_process() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    let (first_sender, first_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(pid, move |outcome| {
        let _ = first_sender.send(outcome);
    })?;
    assert_eq!(runtime.nif_state().monitor_installations.len(), 1);

    runtime.force_next_monitor_spawn_failure_for_test();
    let (duplicate_sender, duplicate_receiver) = mpsc::channel();
    let duplicate_error = runtime
        .monitor_process_for_test(pid, move |outcome| {
            let _ = duplicate_sender.send(outcome);
        })
        .err()
        .ok_or("duplicate monitor installation unexpectedly succeeded")?;

    assert!(duplicate_error.to_string().contains("already has"));
    assert!(
        runtime.is_live(pid),
        "duplicate installation killed the process"
    );
    assert_eq!(runtime.nif_state().monitor_installations.len(), 1);
    runtime.cancel_pid(pid)?;
    let first_outcome = first_receiver.recv_timeout(Duration::from_secs(10))??;
    match first_outcome {
        WorkflowProcessOutcome::Failed(error) => {
            assert!(error.message.contains("Kill"));
        }
        WorkflowProcessOutcome::Completed(_) => {
            return Err("killed process was reported as completed".into());
        }
    }
    assert!(duplicate_receiver.try_recv().is_err());
    for _ in 0..100 {
        if runtime.nif_state().monitor_installations.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(runtime.nif_state().monitor_installations.is_empty());
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn absent_process_without_tombstone_aborts_without_waiting_for_outcome() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let started = Instant::now();

    runtime.abort_unmonitored_process(9_999)?;

    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(runtime.process_cleanup_observed_for_test(9_999));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn unmonitored_abort_bound_exhaustion_is_typed() -> TestResult {
    let signal_delivery = SignalDeliveryConfig::new(
        Duration::from_millis(10),
        1,
        Duration::from_millis(1),
        Duration::from_millis(1),
    );
    let runtime = Arc::new(RuntimeHandle::new(
        RuntimeConfig::new(Some(1)).with_signal_delivery(signal_delivery),
    )?);
    let pid = runtime.spawn_test_process()?;
    let held_process = runtime
        .scheduler
        .process_table()
        .get(pid)
        .ok_or("spawned process was absent before bounded abort")?;
    let started = Instant::now();

    runtime.force_next_monitor_spawn_failure_for_test();
    let error = runtime
        .monitor_process_for_test(pid, |_| {})
        .err()
        .ok_or("held process removal did not exhaust failed-install abort bound")?;

    assert!(matches!(
        error,
        crate::EngineError::Runtime { reason }
            if reason.contains("did not complete unmonitored abort within 10ms")
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(runtime.nif_state().monitor_installations.len(), 1);
    drop(held_process);
    for _ in 0..1_000 {
        if runtime.process_cleanup_observed_for_test(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(runtime.process_cleanup_observed_for_test(pid));
    assert!(runtime.nif_state().monitor_installations.is_empty());

    let typed_pid = runtime.spawn_test_process()?;
    let typed_held_process = runtime
        .scheduler
        .process_table()
        .get(typed_pid)
        .ok_or("second process was absent before typed abort")?;
    let typed_timeout = runtime
        .abort_unmonitored_process(typed_pid)
        .err()
        .ok_or("held process did not return a typed timeout")?;
    assert!(matches!(
        typed_timeout,
        UnmonitoredProcessAbortError::TimedOut {
            process_id,
            timeout_millis: 10
        } if process_id == typed_pid
    ));
    drop(typed_held_process);
    for _ in 0..1_000 {
        if runtime.process_cleanup_observed_for_test(typed_pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(runtime.process_cleanup_observed_for_test(typed_pid));
    runtime.shutdown()?;
    Ok(())
}
