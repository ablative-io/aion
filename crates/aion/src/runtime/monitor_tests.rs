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
    assert_eq!(runtime.nif_state().monitor_installations.len(), 1);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn abort_refuses_a_process_owned_by_a_committed_monitor() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    let (sender, receiver) = mpsc::channel();
    runtime.monitor_process_for_test(pid, move |outcome| {
        let _ = sender.send(outcome);
    })?;

    let abort_error = runtime
        .abort_unmonitored_process(pid)
        .err()
        .ok_or("abort accepted a process with a committed monitor owner")?;

    assert!(matches!(
        abort_error,
        UnmonitoredProcessAbortError::MonitorInstalled { process_id }
            if process_id == pid
    ));
    assert!(runtime.is_live(pid));
    assert!(runtime.abort_jobs.is_empty());
    runtime.cancel_pid(pid)?;
    let _ = receiver.recv_timeout(Duration::from_secs(10))??;
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn duplicate_installation_after_fast_exit_is_sticky() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    let (first_sender, first_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(pid, move |outcome| {
        let _ = first_sender.send(outcome);
    })?;
    runtime.cancel_pid(pid)?;
    let _ = first_receiver.recv_timeout(Duration::from_secs(10))??;
    assert_eq!(runtime.exit_outcome_consumptions_for_test(pid)?, 1);

    let duplicate_error = runtime
        .monitor_process_for_test(pid, |_| {})
        .err()
        .ok_or("completed monitor installation did not keep its sticky claim")?;

    assert!(duplicate_error.to_string().contains("already has"));
    assert_eq!(runtime.exit_outcome_consumptions_for_test(pid)?, 1);
    assert_eq!(runtime.nif_state().monitor_installations.len(), 1);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn dead_and_unavailable_outcome_is_typed_and_runs_full_cleanup() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.pause_next_exit_observer_for_test();
    let pid = runtime.spawn_test_process()?;
    runtime.wait_for_exit_observer_pause_for_test(pid)?;
    runtime.deliver_activity_completion_message_with_attempt(
        pid,
        "activity:71",
        String::from(r#"{"late":true}"#),
        Some(4),
    )?;
    assert!(!runtime.process_cleanup_observed_for_test(pid));
    runtime.cancel_pid(pid)?;

    let (sender, receiver) = mpsc::channel();
    runtime.monitor_process_for_test(pid, move |outcome| {
        let _ = sender.send(outcome);
    })?;
    runtime.force_exit_outcome_unavailable_for_test(pid)?;

    let error = receiver
        .recv_timeout(Duration::from_secs(10))?
        .err()
        .ok_or("evicted exit outcome was not reported as a typed error")?;
    assert!(matches!(
        error,
        crate::EngineError::ProcessExitUnavailable { process_id } if process_id == pid
    ));
    assert_eq!(runtime.exit_outcome_consumptions_for_test(pid)?, 0);
    assert!(runtime.process_cleanup_observed_for_test(pid));
    assert_eq!(runtime.retained_activity_completions(), 0);
    assert_eq!(runtime.retained_activity_attempt_count_for_test(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn exact_cleanup_predicate_rejects_a_live_native_entry() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    runtime.observe_native_entry_for_test(pid);

    assert!(!runtime.process_cleanup_observed_for_test(pid));
    runtime.abort_unmonitored_process(pid)?;
    assert!(runtime.process_cleanup_observed_for_test(pid));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn unavailable_cleanup_executor_is_typed_and_keeps_one_abort_identity() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    runtime.shutdown_cleanup_executor_for_test()?;

    let first = runtime
        .abort_unmonitored_process(pid)
        .err()
        .ok_or("closed cleanup executor accepted an abort")?;
    let retry = runtime
        .abort_unmonitored_process(pid)
        .err()
        .ok_or("closed cleanup executor retry lost its terminal abort identity")?;

    assert!(matches!(
        first,
        UnmonitoredProcessAbortError::ExecutorUnavailable { process_id }
            if process_id == pid
    ));
    assert!(matches!(
        retry,
        UnmonitoredProcessAbortError::ExecutorUnavailable { process_id }
            if process_id == pid
    ));
    assert!(runtime.is_live(pid));
    assert_eq!(runtime.abort_jobs.len(), 1);
    assert!(!runtime.process_cleanup_observed_for_test(pid));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn bounded_cleanup_queue_exhaustion_is_typed() -> TestResult {
    let signal_delivery = SignalDeliveryConfig::new(
        Duration::from_millis(10),
        1,
        Duration::from_millis(1),
        Duration::from_millis(1),
    );
    let runtime = Arc::new(RuntimeHandle::new(
        RuntimeConfig::new(Some(1)).with_signal_delivery(signal_delivery),
    )?);
    let blocked_pid = runtime.spawn_test_process()?;
    let queued_pid = runtime.spawn_test_process()?;
    let exhausted_pid = runtime.spawn_test_process()?;
    let held_process = runtime
        .scheduler
        .process_table()
        .get(blocked_pid)
        .ok_or("blocked process was absent before cleanup queue test")?;

    let blocked = runtime
        .abort_unmonitored_process(blocked_pid)
        .err()
        .ok_or("blocked cleanup job did not exhaust its observation bound")?;
    let queued = runtime
        .abort_unmonitored_process(queued_pid)
        .err()
        .ok_or("queued cleanup job did not exhaust its observation bound")?;
    let exhausted = runtime
        .abort_unmonitored_process(exhausted_pid)
        .err()
        .ok_or("full cleanup queue accepted a third distinct abort job")?;

    assert!(matches!(
        blocked,
        UnmonitoredProcessAbortError::TimedOut { .. }
    ));
    assert!(matches!(
        queued,
        UnmonitoredProcessAbortError::TimedOut { .. }
    ));
    assert!(matches!(
        exhausted,
        UnmonitoredProcessAbortError::ExecutorExhausted { process_id }
            if process_id == exhausted_pid
    ));
    assert!(runtime.is_live(exhausted_pid));
    drop(held_process);
    for _ in 0..1_000 {
        if runtime.process_cleanup_observed_for_test(queued_pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(runtime.process_cleanup_observed_for_test(blocked_pid));
    assert!(runtime.process_cleanup_observed_for_test(queued_pid));
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
