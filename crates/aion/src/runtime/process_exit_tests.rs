//! Singleton process-exit drainer contract tests.

use std::collections::HashSet;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use beamr::scheduler::EXIT_EVENT_CAPACITY;

use super::ProcessExitRegistry;
use crate::EngineError;
use crate::runtime::{RuntimeConfig, RuntimeHandle};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn runtime_claims_the_only_exit_event_subscription_with_typed_duplicate_failure() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    assert!(runtime.scheduler.subscribe_exit_events().is_none());

    let duplicate = ProcessExitRegistry::new(
        Arc::clone(&runtime.scheduler),
        runtime.signal_delivery().cleanup_shutdown_timeout(),
    )
    .err()
    .ok_or("a second process-exit registry claimed the scheduler subscription")?;

    assert!(matches!(
        duplicate,
        EngineError::ProcessExitSubscriptionUnavailable
    ));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn lagged_stream_resynchronizes_every_registered_outcome() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    runtime.process_exits.pause_for_test();

    let first_pid = runtime.spawn_test_process()?;
    runtime.cancel_pid(first_pid)?;
    runtime
        .process_exits
        .wait_for_pause_for_test(Duration::from_secs(10))?;

    let mut pids = Vec::with_capacity(EXIT_EVENT_CAPACITY + 2);
    pids.push(first_pid);
    for _ in 0..=EXIT_EVENT_CAPACITY {
        let pid = runtime.spawn_test_process()?;
        runtime.cancel_pid(pid)?;
        pids.push(pid);
    }

    let (sender, receiver) = mpsc::channel();
    for pid in &pids {
        let callback_sender = sender.clone();
        let callback_pid = *pid;
        runtime.monitor_process_for_test(*pid, move |outcome| {
            let _ = callback_sender.send((callback_pid, outcome.is_ok()));
        })?;
    }
    drop(sender);
    runtime.process_exits.release_for_test();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut observed = HashSet::with_capacity(pids.len());
    while observed.len() < pids.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (pid, outcome_available) = receiver.recv_timeout(remaining)?;
        if !outcome_available {
            return Err(format!("process {pid} did not receive its cached exit outcome").into());
        }
        observed.insert(pid);
    }

    wait_until(deadline, || {
        runtime.process_exits.lag_recoveries_for_test() > 0
            && pids
                .iter()
                .all(|pid| runtime.process_cleanup_complete_for_test(*pid))
    })?;
    assert_eq!(observed.len(), pids.len());
    assert!(runtime.process_exits.lag_recoveries_for_test() > 0);
    runtime.shutdown()?;
    Ok(())
}

fn wait_until(deadline: Instant, predicate: impl Fn() -> bool) -> TestResult {
    while Instant::now() < deadline {
        if predicate() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err("process-exit condition did not become true before its deadline".into())
}
