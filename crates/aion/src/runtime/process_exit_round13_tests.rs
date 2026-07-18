//! Round-13 regressions for shutdown union draining and bounded callback retry.

use std::collections::HashSet;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

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
    Err("round-13 process-exit condition missed its deadline".into())
}

#[test]
fn shutdown_drains_parked_release_only_children_to_baseline() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(2)))?;
    runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;
    let baseline_children = runtime.process_exits.unobserved_children_for_test()?;
    let baseline_processes = runtime.live_processes_for_test();

    let parent = runtime.spawn_workflow(
        "aion_fixture_workflow",
        "parked_children",
        RuntimeInput::default(),
    )?;
    let _ = runtime.process_exit_for_test(parent)?;
    wait_until(Instant::now() + Duration::from_secs(10), || {
        matches!(
            runtime.process_exits.unobserved_children_for_test(),
            Ok(children) if children == baseline_children + 64
        ) && runtime.live_processes_for_test() == baseline_processes + 64
    })?;

    runtime.shutdown()?;
    assert_eq!(
        runtime.process_exits.unobserved_children_for_test()?,
        baseline_children
    );
    assert_eq!(runtime.live_processes_for_test(), baseline_processes);
    Ok(())
}

#[test]
fn full_callback_queue_retains_bounded_automatic_retries() -> TestResult {
    let delivery = SignalDeliveryConfig::new(
        Duration::from_secs(1),
        1,
        Duration::from_millis(1),
        Duration::from_millis(1),
    );
    let runtime = Arc::new(RuntimeHandle::new(
        RuntimeConfig::new(Some(1)).with_signal_delivery(delivery),
    )?);
    let pids = [
        runtime.spawn_test_process()?,
        runtime.spawn_test_process()?,
        runtime.spawn_test_process()?,
        runtime.spawn_test_process()?,
    ];
    let (completed_sender, completed_receiver) = mpsc::channel();
    let (entered_sender, entered_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let first_sender = completed_sender.clone();
    let first_pid = pids[0];
    runtime.monitor_process_for_test(first_pid, move |_| {
        let _ = entered_sender.send(());
        let _ = release_receiver.recv();
        let _ = first_sender.send(first_pid);
    })?;
    for &pid in &pids[1..] {
        let sender = completed_sender.clone();
        runtime.monitor_process_for_test(pid, move |_| {
            let _ = sender.send(pid);
        })?;
    }
    drop(completed_sender);

    runtime.cancel_pid(pids[0])?;
    entered_receiver.recv_timeout(Duration::from_secs(10))?;
    runtime.cancel_pid(pids[1])?;
    wait_until(Instant::now() + Duration::from_secs(10), || {
        runtime.process_exits.callback_queue_usage_for_test() == (1, 1)
    })?;
    runtime.cancel_pid(pids[2])?;
    runtime.cancel_pid(pids[3])?;
    wait_until(Instant::now() + Duration::from_secs(10), || {
        matches!(
            runtime.process_exits.pending_callbacks_for_test(),
            Ok(pending) if pending >= 2
        )
    })?;
    let (queued, capacity) = runtime.process_exits.callback_queue_usage_for_test();
    assert_eq!(capacity, 1);
    assert!(queued <= capacity);

    release_sender.send(())?;
    let mut completed = HashSet::new();
    for _ in &pids {
        let pid = completed_receiver.recv_timeout(Duration::from_secs(10))?;
        assert!(completed.insert(pid), "callback {pid} ran more than once");
    }
    assert_eq!(completed, HashSet::from(pids));
    assert!(completed_receiver.try_recv().is_err());
    wait_until(Instant::now() + Duration::from_secs(10), || {
        matches!(runtime.process_exits.pending_callbacks_for_test(), Ok(0))
            && runtime.process_exits.len() == 0
    })?;
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn committed_callback_admission_hands_off_before_dispatcher_shutdown() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    runtime.cancel_pid(pid)?;
    wait_until(Instant::now() + Duration::from_secs(10), || {
        matches!(runtime.process_exits.has_terminal(pid), Ok(true))
    })?;
    runtime.process_exits.pause_next_callback_admission();

    let (callback_sender, callback_receiver) = mpsc::channel();
    let monitor_runtime = Arc::clone(&runtime);
    let (monitor_sender, monitor_receiver) = mpsc::channel();
    let monitor = std::thread::spawn(move || {
        let result = monitor_runtime
            .monitor_process_for_test(pid, move |_| {
                let _ = callback_sender.send(pid);
            })
            .map(|_| ());
        let _ = monitor_sender.send(result);
    });
    runtime
        .process_exits
        .wait_for_callback_admission_pause(Duration::from_secs(10))?;

    let shutdown_runtime = Arc::clone(&runtime);
    let (shutdown_sender, shutdown_receiver) = mpsc::channel();
    let shutdown = std::thread::spawn(move || {
        let _ = shutdown_sender.send(shutdown_runtime.shutdown());
    });
    std::thread::sleep(Duration::from_millis(10));
    assert!(shutdown_receiver.try_recv().is_err());

    runtime.process_exits.release_callback_admission();
    monitor_receiver.recv_timeout(Duration::from_secs(10))??;
    assert_eq!(
        callback_receiver.recv_timeout(Duration::from_secs(10))?,
        pid
    );
    assert!(callback_receiver.try_recv().is_err());
    shutdown_receiver.recv_timeout(Duration::from_secs(10))??;
    monitor
        .join()
        .map_err(|_| "monitor admission thread terminated unexpectedly")?;
    shutdown
        .join()
        .map_err(|_| "runtime shutdown thread terminated unexpectedly")?;
    Ok(())
}
