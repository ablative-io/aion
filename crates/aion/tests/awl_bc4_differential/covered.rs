//! Deliverable 2 + 5: the differential over the covered ratchet. Every fixture
//! both backends accept runs through a real engine; the normalized durable
//! trails must be byte-identical, and the exact outcome inventory is pinned so
//! any drift — a fixture falling from completed to parked, an intersection
//! erosion, or an infrastructure defect — fails loudly instead of quiescing.

use crate::driver::run_differential;
use crate::fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The exact covered corpus size (`aion-awl/src/mir/covered.rs`).
const COVERED_COUNT: usize = 76;

/// The fixtures that park at a durable TIMER (a `sleep 30s` or a
/// `wait … timeout`) — proven by a pending `TimerStarted`. Pinned by name.
const EXPECTED_TIMER_PARKED: &[&str] = &[
    "loop-outcomes/valid/guard_optional_wait",
    "step-bodies/valid/wait_and_sleep",
    "step-bodies/valid/wait_timeout_optional",
];

/// The fixtures that park at a bare SIGNAL wait (no timeout timer) — quiescent
/// after their last activity, waiting on a signal that never arrives.
const EXPECTED_SIGNAL_PARKED: &[&str] = &[
    "header-types/valid/combined",
    "header-types/valid/signal_wait",
];

/// The covered fixtures that terminate in `WorkflowFailed` — BYTE-IDENTICALLY on
/// both backends — under the minimal schema-valid inputs and activity results
/// the harness feeds. Two honest kinds, pinned so any success/failure flip is
/// loud:
/// 1. Data-driven error outcomes: the workflow runs its real body (activities
///    decode and complete) and reaches a `route failure` / else branch because
///    the minimal data takes it there (a `score 0.0 < 0.5` guard, an empty
///    collection, a zero-count loop).
/// 2. Child-spawning fixtures whose synthesized child workflow cannot be
///    satisfied by the harness's parent-scoped canned results, so the awaited
///    child errors and the parent fails at that boundary. The differential's
///    byte-identity still holds; the limitation is that the child's real body
///    is not exercised end to end.
const EXPECTED_FAILED: &[&str] = &[
    "dag-fork/valid/fork_named_branches",
    "declarations/valid/call_site_override",
    "declarations/valid/child_call_awaited",
    "declarations/valid/child_spawn_combo",
    "declarations/valid/declarations_combined",
    "declarations/valid/spawn_detached",
    "declarations/valid/worker_action_config_lines",
    "header-types/valid/builtins",
    "header-types/valid/zero_inputs",
    "loop-outcomes/valid/backward_route_bounded_cycle",
    "loop-outcomes/valid/float_threshold_guard",
    "loop-outcomes/valid/fork_in_loop_live_ins",
    "loop-outcomes/valid/loop_after_fall_through",
    "loop-outcomes/valid/loop_compound_until_nested",
    "loop-outcomes/valid/loop_counting_until_max",
    "loop-outcomes/valid/loop_without_counting",
    "loop-outcomes/valid/route_outcome_by_name",
    "schema-doors/valid/import_constraints",
    "schema-doors/valid/import_nested_defs",
    "schema-doors/valid/import_ticket",
    "schema-doors/valid/inline_schema_round",
    "step-bodies/valid/collection_predicates",
    "step-bodies/valid/index_and_concat",
    "step-bodies/valid/literal_forms",
    "step-bodies/valid/predicates_and_operators",
];

/// Flagship de-risk: the first AWL workflow, end to end through both backends,
/// must COMPLETE its real greet -> shout path with a byte-identical trail.
#[tokio::test(flavor = "multi_thread")]
async fn flagship_awl_hello_matches() -> TestResult {
    let names = vec![String::from("flagship/valid/awl_hello")];
    let report = run_differential(&names, "flagship").await?;
    assert!(
        report.infra.is_empty() && report.divergences.is_empty() && report.refusals.is_empty(),
        "flagship not clean:\n{}",
        report.render()
    );
    assert_eq!(
        report.succeeded,
        vec![String::from("flagship/valid/awl_hello")],
        "flagship did not complete its real path:\n{}",
        report.render()
    );
    Ok(())
}

/// The full covered differential: build both backends for every covered fixture
/// (ONE `gleam build`), run both, assert byte-identical normalized trails with
/// no divergence and no infra failure, and pin the exact outcome inventory.
#[tokio::test(flavor = "multi_thread")]
async fn covered_set_is_byte_identical() -> TestResult {
    let names = fixtures::covered_paths()?;
    assert_eq!(
        names.len(),
        COVERED_COUNT,
        "covered ratchet size drifted (expected {COVERED_COUNT})"
    );
    let report = run_differential(&names, "covered").await?;
    println!("{}", report.render());
    println!("SUCCEEDED {:?}", report.succeeded);
    println!("FAILED {:?}", report.failed_fixtures());
    println!("TIMER_PARKED {:?}", report.timer_parked_fixtures());
    println!("SIGNAL_PARKED {:?}", report.signal_parked_fixtures());

    assert!(
        report.infra.is_empty(),
        "covered differential hit infrastructure failures:\n{}",
        report.render()
    );
    assert!(
        report.divergences.is_empty(),
        "covered differential found divergences:\n{}",
        report.render()
    );
    assert!(
        report.refusals.is_empty(),
        "covered fixtures unexpectedly refused (intersection shrinkage):\n{}",
        report.render()
    );

    assert_eq!(
        report.timer_parked_fixtures(),
        sorted(EXPECTED_TIMER_PARKED),
        "the set of durable-timer-parked fixtures drifted:\n{}",
        report.render()
    );
    assert_eq!(
        report.signal_parked_fixtures(),
        sorted(EXPECTED_SIGNAL_PARKED),
        "the set of signal-wait-parked fixtures drifted:\n{}",
        report.render()
    );
    assert_eq!(
        report.failed_fixtures(),
        sorted(EXPECTED_FAILED),
        "the set of error-path fixtures drifted:\n{}",
        report.render()
    );
    // 76 = succeeded + failed(error path) + timer-parked + signal-parked. Every
    // covered fixture lands in exactly one, with byte-identical trails.
    assert_eq!(
        report.identical_count(),
        COVERED_COUNT,
        "not every covered fixture produced a byte-identical comparison:\n{}",
        report.render()
    );
    let parked = EXPECTED_TIMER_PARKED.len() + EXPECTED_SIGNAL_PARKED.len();
    assert_eq!(
        report.succeeded.len(),
        COVERED_COUNT - EXPECTED_FAILED.len() - parked,
        "the completed-success count drifted:\n{}",
        report.render()
    );
    Ok(())
}

/// Sorts a static name list into an owned, comparable vector.
fn sorted(names: &[&str]) -> Vec<String> {
    let mut owned: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    owned.sort();
    owned
}
