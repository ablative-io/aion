//! Round-14 regressions for tracked Gate-3 `spawn/1` and `spawn_link/1` fun spawns.

use std::time::{Duration, Instant};

use crate::runtime::{RuntimeConfig, RuntimeHandle, RuntimeInput};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BASELINE_FUN_SPAWN_CHILDREN: usize = 64;
const OVERFLOW_FUN_SPAWN_CHILDREN: usize = 1_100;

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
    Err("round-14 process-exit condition missed its deadline".into())
}

fn wait_for_process_range_to_exit(
    runtime: &RuntimeHandle,
    first_pid: u64,
    last_pid: u64,
) -> TestResult {
    wait_until(Instant::now() + Duration::from_secs(20), || {
        (first_pid..=last_pid).all(|pid| !runtime.is_live(pid))
    })
}

fn assert_exit_artifacts_absent(runtime: &RuntimeHandle, first_pid: u64, last_pid: u64) {
    for pid in first_pid..=last_pid {
        assert!(
            runtime.scheduler.take_exit_outcome(pid).is_none(),
            "process {pid} retained a child exit outcome"
        );
        assert!(
            runtime.scheduler.take_exit_error(pid).is_none(),
            "process {pid} retained child exit diagnostics"
        );
        assert!(
            runtime.scheduler.take_exit_exception(pid).is_none(),
            "process {pid} retained a child exit exception"
        );
    }
}

#[test]
fn fun_spawn_variants_are_release_only_and_return_maps_to_baseline() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(2)))?;
    runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;
    let baseline_children = runtime.process_exits.unobserved_children_for_test()?;
    let baseline_processes = runtime.live_processes_for_test();
    runtime.process_exits.pause_for_test();

    let parent = runtime.spawn_workflow(
        "aion_fixture_workflow",
        "fun_spawn_children",
        RuntimeInput::default(),
    )?;
    runtime
        .process_exits
        .wait_for_pause_for_test(Duration::from_secs(10))?;
    let last_child = parent + u64::try_from(BASELINE_FUN_SPAWN_CHILDREN)?;
    wait_for_process_range_to_exit(&runtime, parent, last_child)?;
    assert_eq!(
        runtime.process_exits.unobserved_children_for_test()?,
        baseline_children + BASELINE_FUN_SPAWN_CHILDREN,
        "both Gate-3 fun-spawn variants must classify every child before exit observation"
    );

    runtime.process_exits.release_for_test();
    let _ = runtime.process_exit_for_test(parent)?;
    wait_until(Instant::now() + Duration::from_secs(10), || {
        matches!(
            runtime.process_exits.unobserved_children_for_test(),
            Ok(count) if count == baseline_children
        ) && runtime.live_processes_for_test() == baseline_processes
    })?;
    assert_exit_artifacts_absent(&runtime, parent + 1, last_child);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn shutdown_union_drains_parked_fun_spawn_children_to_baseline() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(2)))?;
    runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;
    let baseline_children = runtime.process_exits.unobserved_children_for_test()?;
    let baseline_processes = runtime.live_processes_for_test();

    let parent = runtime.spawn_workflow(
        "aion_fixture_workflow",
        "parked_fun_spawn_children",
        RuntimeInput::default(),
    )?;
    let _ = runtime.process_exit_for_test(parent)?;
    wait_until(Instant::now() + Duration::from_secs(10), || {
        matches!(
            runtime.process_exits.unobserved_children_for_test(),
            Ok(children) if children == baseline_children + BASELINE_FUN_SPAWN_CHILDREN
        ) && runtime.live_processes_for_test() == baseline_processes + BASELINE_FUN_SPAWN_CHILDREN
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
fn overflow_recovery_releases_both_fun_spawn_variants_to_baseline() -> TestResult {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(2)))?;
    runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;
    let baseline_children = runtime.process_exits.unobserved_children_for_test()?;
    let baseline_processes = runtime.live_processes_for_test();
    let baseline_recoveries = runtime.process_exits.lag_recoveries_for_test();
    runtime.process_exits.pause_for_test();

    let parent = runtime.spawn_workflow(
        "aion_fixture_workflow",
        "overflow_fun_spawn_children",
        RuntimeInput::default(),
    )?;
    runtime
        .process_exits
        .wait_for_pause_for_test(Duration::from_secs(10))?;
    let last_child = parent + u64::try_from(OVERFLOW_FUN_SPAWN_CHILDREN)?;
    wait_for_process_range_to_exit(&runtime, parent, last_child)?;
    assert_eq!(
        runtime.process_exits.unobserved_children_for_test()?,
        baseline_children + OVERFLOW_FUN_SPAWN_CHILDREN,
        "overflow recovery requires both fun-spawn variants in the recoverable child set"
    );

    runtime.process_exits.release_for_test();
    let _ = runtime.process_exit_for_test(parent)?;
    wait_until(Instant::now() + Duration::from_secs(20), || {
        runtime.process_exits.lag_recoveries_for_test() > baseline_recoveries
            && matches!(
                runtime.process_exits.unobserved_children_for_test(),
                Ok(count) if count == baseline_children
            )
            && runtime.live_processes_for_test() == baseline_processes
    })?;
    assert_exit_artifacts_absent(&runtime, parent + 1, last_child);
    runtime.shutdown()?;
    Ok(())
}
