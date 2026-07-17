//! Runtime process monitor installation and rollback tests.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aion_core::{ContentType, Payload};

use super::{UnmonitoredProcessAbortError, WorkflowProcessOutcome};
use crate::runtime::{RuntimeConfig, RuntimeHandle, SignalDeliveryConfig};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn wait_for_process_cleanup(runtime: &RuntimeHandle, pid: u64) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if runtime.process_cleanup_complete_for_test(pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err(
        format!("process {pid} cleanup did not reach its terminal predicate before the deadline")
            .into(),
    )
}

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
fn monitor_installation_failure_drains_retained_completion_transaction() -> TestResult {
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

    runtime.force_next_monitor_installation_failure_for_test();
    let error = runtime
        .monitor_process_for_test(pid, |_| {})
        .err()
        .ok_or("forced monitor installation failure installed a monitor")?;

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

    runtime.force_next_monitor_installation_failure_for_test();
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
    wait_for_process_cleanup(&runtime, pid)?;
    assert!(runtime.nif_state().monitor_installations.is_empty());
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
fn duplicate_installation_after_retirement_is_typed_terminal() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    let (first_sender, first_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(pid, move |outcome| {
        let _ = first_sender.send(outcome);
    })?;
    runtime.cancel_pid(pid)?;
    let _ = first_receiver.recv_timeout(Duration::from_secs(10))??;
    wait_for_process_cleanup(&runtime, pid)?;

    let duplicate_error = runtime
        .monitor_process_for_test(pid, |_| {})
        .err()
        .ok_or("retired process accepted a fresh monitor installation")?;

    assert!(matches!(
        duplicate_error,
        crate::EngineError::ProcessExitAlreadyTerminal { process_id } if process_id == pid
    ));
    assert!(runtime.nif_state().monitor_installations.is_empty());
    assert_eq!(runtime.process_exits.len(), 0);
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
    assert!(!runtime.process_cleanup_complete_for_test(pid));
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
    wait_for_process_cleanup(&runtime, pid)?;
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

    assert!(!runtime.process_cleanup_complete_for_test(pid));
    runtime.abort_unmonitored_process(pid)?;
    assert!(runtime.process_cleanup_complete_for_test(pid));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn unavailable_cleanup_executor_is_typed_without_sticky_abort_identity() -> TestResult {
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
        .ok_or("closed cleanup executor accepted an abort retry")?;

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
    assert!(runtime.abort_jobs.is_empty());
    assert!(!runtime.process_cleanup_complete_for_test(pid));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn bounded_cleanup_queue_exhaustion_is_typed() -> TestResult {
    let signal_delivery = SignalDeliveryConfig::new(
        Duration::from_millis(100),
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
    runtime.force_next_monitor_installation_failure_for_test();
    let exhausted = runtime
        .monitor_process_for_test(exhausted_pid, |_| {})
        .err()
        .ok_or("full cleanup queue accepted failed-installation rollback")?;

    assert!(matches!(
        blocked,
        UnmonitoredProcessAbortError::TimedOut { .. }
    ));
    assert!(matches!(
        queued,
        UnmonitoredProcessAbortError::TimedOut { .. }
    ));
    assert!(matches!(exhausted, crate::EngineError::Runtime { reason }
        if reason.contains("cleanup executor is exhausted")));
    assert!(runtime.is_live(exhausted_pid));
    assert!(!runtime.abort_jobs.contains_key(&exhausted_pid));
    assert!(
        !runtime
            .nif_state()
            .monitor_installations
            .contains_key(&exhausted_pid)
    );
    drop(held_process);
    wait_for_process_cleanup(&runtime, blocked_pid)?;
    wait_for_process_cleanup(&runtime, queued_pid)?;
    runtime.abort_unmonitored_process(exhausted_pid)?;
    assert!(!runtime.is_live(exhausted_pid));
    wait_for_process_cleanup(&runtime, exhausted_pid)?;
    assert!(runtime.abort_jobs.is_empty());
    assert_eq!(runtime.process_exits.len(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn absent_process_without_tombstone_aborts_without_waiting_for_outcome() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let started = Instant::now();

    runtime.abort_unmonitored_process(9_999)?;

    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(runtime.process_cleanup_complete_for_test(9_999));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn shutdown_force_unblocks_process_exit_jobs_before_executor_join() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    let record = runtime.process_exits.get(pid)?;
    runtime
        .cleanup_executor
        .submit(Box::new(move || {
            if record.wait().is_ok() {
                let _ = record.close_without_monitor();
            }
        }))
        .map_err(|error| format!("cleanup wait job was refused: {error:?}"))?;

    runtime.shutdown()?;

    assert!(!runtime.is_live(pid));
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

    runtime.force_next_monitor_installation_failure_for_test();
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
    wait_for_process_cleanup(&runtime, pid)?;
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
    wait_for_process_cleanup(&runtime, typed_pid)?;
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn completed_process_lifecycle_state_returns_to_baseline_under_churn() -> TestResult {
    const WORKFLOWS: usize = 24;

    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let baseline_records = runtime.process_exits.len();
    let baseline_claims = runtime.nif_state().monitor_installations.len();
    let baseline_aborts = runtime.abort_jobs.len();

    for _ in 0..WORKFLOWS {
        let pid = runtime.spawn_test_process()?;
        let (sender, receiver) = mpsc::sync_channel(1);
        runtime.monitor_process_for_test(pid, move |outcome| {
            let _ = sender.send(outcome);
        })?;
        runtime.cancel_pid(pid)?;
        let _ = receiver.recv_timeout(Duration::from_secs(10))??;
        wait_for_process_cleanup(&runtime, pid)?;
    }

    for _ in 0..WORKFLOWS {
        let pid = runtime.spawn_test_process()?;
        match runtime.abort_unmonitored_process(pid) {
            Ok(()) | Err(UnmonitoredProcessAbortError::TimedOut { .. }) => {}
            Err(error) => {
                return Err(format!("process {pid} churn abort failed: {error}").into());
            }
        }
        wait_for_process_cleanup(&runtime, pid)?;
    }

    for _ in 0..WORKFLOWS {
        let pid = runtime.spawn_test_process()?;
        runtime.cancel_pid(pid)?;
        let _ = runtime.activity_process_exit_outcome(pid)?;
        assert!(!runtime.process_exits.contains(pid));
    }

    assert_eq!(runtime.process_exits.len(), baseline_records);
    assert_eq!(
        runtime.nif_state().monitor_installations.len(),
        baseline_claims
    );
    assert_eq!(runtime.abort_jobs.len(), baseline_aborts);
    runtime.shutdown()?;
    Ok(())
}
