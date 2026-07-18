//! Round-12 regressions for reservations, child outcome release, and callback isolation.

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use beamr::scheduler::EXIT_EVENT_CAPACITY;

use crate::EngineError;
use crate::runtime::{RuntimeConfig, RuntimeHandle, RuntimeInput, SignalDeliveryConfig};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture_workflow_beam() -> &'static [u8] {
    include_bytes!("../../tests/fixtures/aion_fixture_workflow.beam")
}

fn wait_until(deadline: Instant, predicate: impl Fn() -> bool) -> TestResult {
    while Instant::now() < deadline {
        if predicate() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err("round-12 process-exit condition missed its deadline".into())
}

#[test]
fn lag_recovery_waits_for_spawn_reservation_before_snapshot() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.process_exits.pause_for_test();
    let first_pid = runtime.spawn_test_process()?;
    runtime.cancel_pid(first_pid)?;
    runtime
        .process_exits
        .wait_for_pause_for_test(Duration::from_secs(10))?;

    for _ in 0..=EXIT_EVENT_CAPACITY {
        let pid = runtime.spawn_test_process()?;
        runtime.cancel_pid(pid)?;
    }

    runtime.process_exits.pause_next_registration();
    let spawn_runtime = Arc::clone(&runtime);
    let spawn = std::thread::spawn(move || spawn_runtime.spawn_test_process());
    let fast_pid = runtime
        .process_exits
        .wait_for_registration_pause(Duration::from_secs(10))?;
    runtime.cancel_pid(fast_pid)?;
    runtime.process_exits.release_for_test();
    runtime.process_exits.release_registration();
    let registered_pid = spawn
        .join()
        .map_err(|_| "reserved spawn thread terminated unexpectedly")??;
    assert_eq!(registered_pid, fast_pid);

    let (sender, receiver) = mpsc::channel();
    runtime.monitor_process_for_test(fast_pid, move |outcome| {
        let _ = sender.send(outcome.is_ok());
    })?;
    assert!(receiver.recv_timeout(Duration::from_secs(10))?);
    assert!(receiver.try_recv().is_err());
    wait_until(Instant::now() + Duration::from_secs(10), || {
        !runtime.process_exits.contains(fast_pid)
    })?;

    let shutdown_started = Instant::now();
    runtime.shutdown()?;
    assert!(shutdown_started.elapsed() < Duration::from_secs(10));
    Ok(())
}

#[test]
fn beam_spawn_children_release_outcomes_to_baseline() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(2)))?;
    runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;
    let baseline = runtime.process_exits.unobserved_children_for_test()?;

    let parent = runtime.spawn_workflow(
        "aion_fixture_workflow",
        "spawn_children",
        RuntimeInput::default(),
    )?;
    let _ = runtime.process_exit_for_test(parent)?;
    wait_until(Instant::now() + Duration::from_secs(10), || {
        matches!(
            runtime.process_exits.unobserved_children_for_test(),
            Ok(count) if count == baseline
        )
    })?;

    assert_eq!(
        runtime.process_exits.unobserved_children_for_test()?,
        baseline
    );
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn lag_recovery_releases_beam_spawn_children_to_baseline() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(2)))?;
    runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;
    let baseline = runtime.process_exits.unobserved_children_for_test()?;
    runtime.process_exits.pause_for_test();

    let parent = runtime.spawn_workflow(
        "aion_fixture_workflow",
        "overflow_children",
        RuntimeInput::default(),
    )?;
    runtime
        .process_exits
        .wait_for_pause_for_test(Duration::from_secs(10))?;
    wait_until(Instant::now() + Duration::from_secs(20), || {
        !runtime.is_live(parent)
    })
    .map_err(|_| "overflow parent remained live")?;
    let last_child = parent + 1_100;
    wait_until(Instant::now() + Duration::from_secs(20), || {
        (parent + 1..=last_child).all(|pid| !runtime.is_live(pid))
    })
    .map_err(|_| "overflow children remained live")?;
    runtime.process_exits.release_for_test();
    let _ = runtime.process_exit_for_test(parent)?;
    wait_until(Instant::now() + Duration::from_secs(20), || {
        runtime.process_exits.lag_recoveries_for_test() > 0
            && matches!(
                runtime.process_exits.unobserved_children_for_test(),
                Ok(count) if count == baseline
            )
    })
    .map_err(|_| {
        format!(
            "overflow recovery stalled; lag recoveries={}",
            runtime.process_exits.lag_recoveries_for_test()
        )
    })?;

    assert_eq!(
        runtime.process_exits.unobserved_children_for_test()?,
        baseline
    );
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn blocking_callback_cannot_backpressure_drainer_and_shutdown_is_retryable() -> TestResult {
    let delivery = SignalDeliveryConfig::new(
        Duration::from_millis(50),
        1,
        Duration::from_millis(1),
        Duration::from_millis(1),
    );
    let runtime = Arc::new(RuntimeHandle::new(
        RuntimeConfig::new(Some(1)).with_signal_delivery(delivery),
    )?);
    let blocked_pid = runtime.spawn_test_process()?;
    let observed_pid = runtime.spawn_test_process()?;
    let (entered_sender, entered_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(blocked_pid, move |_| {
        let _ = entered_sender.send(());
        let _ = release_receiver.recv();
    })?;
    let (observed_sender, observed_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(observed_pid, move |outcome| {
        let _ = observed_sender.send(outcome.is_ok());
    })?;

    runtime.cancel_pid(blocked_pid)?;
    entered_receiver.recv_timeout(Duration::from_secs(10))?;
    runtime.cancel_pid(observed_pid)?;
    wait_until(Instant::now() + Duration::from_secs(10), || {
        matches!(runtime.process_exits.has_terminal(observed_pid), Ok(true))
    })?;

    let shutdown_started = Instant::now();
    let first_shutdown = runtime
        .shutdown()
        .err()
        .ok_or("blocked callback did not produce bounded shutdown failure")?;
    assert!(matches!(
        first_shutdown,
        EngineError::ProcessExitCallbackDispatcherShutdownTimedOut { .. }
    ));
    assert!(shutdown_started.elapsed() < Duration::from_secs(2));
    release_sender.send(())?;
    assert!(observed_receiver.recv_timeout(Duration::from_secs(10))?);
    assert!(observed_receiver.try_recv().is_err());
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn panicking_callback_cannot_terminate_singleton_drainer() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let panicking_pid = runtime.spawn_test_process()?;
    let observed_pid = runtime.spawn_test_process()?;
    let (panic_entered_sender, panic_entered_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(panicking_pid, move |_| {
        let _ = panic_entered_sender.send(());
        std::panic::resume_unwind(Box::new(String::from("intentional callback panic")));
    })?;
    let (observed_sender, observed_receiver) = mpsc::channel();
    runtime.monitor_process_for_test(observed_pid, move |outcome| {
        let _ = observed_sender.send(outcome.is_ok());
    })?;

    runtime.cancel_pid(panicking_pid)?;
    panic_entered_receiver.recv_timeout(Duration::from_secs(10))?;
    runtime.cancel_pid(observed_pid)?;
    assert!(observed_receiver.recv_timeout(Duration::from_secs(10))?);
    assert!(observed_receiver.try_recv().is_err());
    assert!(runtime.process_exits.has_terminal(observed_pid)?);
    runtime.shutdown()?;
    Ok(())
}
