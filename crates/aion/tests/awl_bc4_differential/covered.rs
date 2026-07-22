//! Deliverable 2 + 5: the differential over the covered ratchet. Every fixture
//! the oracle covers runs through a real engine; the normalized durable trails
//! must be byte-identical, and the EXACT outcome inventory — every name in every
//! bucket — is pinned so any drift (a name swap, a completed->failed flip, a
//! failed->cancelled semantic regression, an intersection erosion, an infra
//! defect) fails loudly instead of quiescing.

use std::collections::BTreeSet;

use crate::driver::run_differential;
use crate::fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The exact covered corpus size (`aion-awl/src/mir/covered.rs`).
const COVERED_COUNT: usize = 76;

/// Out-of-oracle (r2 finding 3): bare signal-wait fixtures whose durable park
/// boundary is NOT observable through any engine surface without racy sampling.
/// `SignalResumeHandoff::pending_count` reports queued signals, not a registered
/// wait, and visibility reports only `Running`; there is no positive
/// signal-park evidence. Per decision-16 (simplification over a racy proof) they
/// are EXCLUDED from the oracle and pinned here, rather than classified from
/// 120ms of history silence. They still lower, emit, and are covered by
/// aion-awl's own ratchet; only their differential RUN is out-of-oracle.
const EXCLUDED_OUT_OF_ORACLE: &[&str] = &[
    "header-types/valid/combined",
    "header-types/valid/signal_wait",
];

/// The fixtures that park at a durable TIMER, proven by visibility `Running`
/// plus this exact pending-timer identity (the first anonymous timer).
const EXPECTED_TIMER_PARKED: &[(&str, &str)] = &[
    (
        "loop-outcomes/valid/guard_optional_wait",
        "timer:anonymous:0",
    ),
    ("step-bodies/valid/wait_and_sleep", "timer:anonymous:0"),
    (
        "step-bodies/valid/wait_timeout_optional",
        "timer:anonymous:0",
    ),
];

/// This corpus records no cancellation; the bucket is asserted empty so a
/// `WorkflowFailed` -> `WorkflowCancelled` regression cannot hide.
const EXPECTED_CANCELLED: &[&str] = &[];

/// The covered fixtures that terminate in `WorkflowFailed` — BYTE-IDENTICALLY on
/// both backends — under the minimal schema-valid inputs and activity results
/// the harness feeds. Two honest kinds, pinned so any flip is loud:
/// 1. Data-driven error outcomes (`AwlOutcomeFailure`): the workflow runs its
///    real body (activities decode and complete) and a `route failure` / else
///    branch is taken because the minimal data goes there (a `score 0.0 < 0.5`
///    guard, an empty collection, a zero-count loop).
/// 2. Child-spawning fixtures (`AwlChildFailed`): the synthesized child workflow
///    type is not registered for spawning in the differential's package, so the
///    parent fails at the child boundary — `child_call_awaited`,
///    `child_spawn_combo`, `spawn_detached`, proven in
///    `child_spawning_fixtures_fail_at_the_child_boundary`.
///    (`declarations_combined` also declares a child but takes a data-driven
///    route-failure first, so it belongs to kind 1.)
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

/// The covered fixtures that COMPLETE their real path (`WorkflowCompleted`).
const EXPECTED_SUCCEEDED: &[&str] = &[
    "dag-fork/valid/after_single",
    "dag-fork/valid/child_collection_fork",
    "dag-fork/valid/child_collection_fork_sequential",
    "dag-fork/valid/empty_fork_collection",
    "dag-fork/valid/fall_through_chain",
    "dag-fork/valid/fork_action_fanout",
    "dag-fork/valid/fork_collection_join",
    "dag-fork/valid/fork_named_homogeneous",
    "dag-fork/valid/fork_sequential",
    "dag-fork/valid/fork_sequential_route",
    "dag-fork/valid/runtime_sized_fork",
    "dag-fork/valid/sit_one",
    "declarations/valid/timeout_inside_retry",
    "declarations/valid/worker_retry_backoff",
    "declarations/valid/worker_single_action",
    "declarations/valid/workers_multiple",
    "ergonomics/valid/flow_vocab_b1",
    "flagship/valid/awl_hello",
    "flow-shape/valid/distribute_activity_tolerant",
    "flow-shape/valid/distribute_child_collect",
    "flow-shape/valid/distribute_child_tolerant",
    "flow-shape/valid/region_pure_decision",
    "flow-shape/valid/sequence_activity_tolerant",
    "flow-shape/valid/sequence_region_loopback",
    "header-types/valid/doc_comments",
    "header-types/valid/enum",
    "header-types/valid/line_width",
    "header-types/valid/max_arity_record",
    "header-types/valid/minimal",
    "header-types/valid/noncanonical_commas",
    "header-types/valid/workflow_timeout",
    "loop-outcomes/valid/enum_when_totality",
    "schema-doors/valid/inline_verbatim_constraints",
    "schema-doors/valid/mixed_doors",
    "schema-doors/valid/optional_shorthand",
    "schema-doors/valid/short_circuit_optional",
    "step-bodies/valid/calls_and_side_effects",
    "step-bodies/valid/combinators",
    "step-bodies/valid/fallible_all_short_circuit",
    "step-bodies/valid/fallible_any_short_circuit",
    "step-bodies/valid/fallible_collection_predicates",
    "step-bodies/valid/general_concat",
    "step-bodies/valid/pipe_chain_stages",
    "step-bodies/valid/step_bodies_combined",
    "step-bodies/valid/unicode_payloads",
    "step-bodies/valid/workflow_id",
];

/// Flagship de-risk: the first AWL workflow completes its real greet -> shout
/// path with a byte-identical trail across both backends.
#[tokio::test(flavor = "multi_thread")]
async fn flagship_awl_hello_matches() -> TestResult {
    let names = vec![String::from("flagship/valid/awl_hello")];
    let report = run_differential(&names, "flagship", &[]).await?;
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
/// (ONE `gleam build`), run both, assert byte-identical normalized trails, and
/// pin the EXACT name inventory of every outcome bucket.
#[tokio::test(flavor = "multi_thread")]
async fn covered_set_is_byte_identical() -> TestResult {
    let names = fixtures::covered_paths()?;
    assert_eq!(
        names.len(),
        COVERED_COUNT,
        "covered ratchet size drifted (expected {COVERED_COUNT})"
    );
    let report = run_differential(&names, "covered", EXCLUDED_OUT_OF_ORACLE).await?;
    println!("{}", report.render());

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

    // Exact per-bucket NAME pins (r2 finding 2).
    assert_eq!(
        report.succeeded_fixtures(),
        sorted(EXPECTED_SUCCEEDED),
        "succeeded set drifted:\n{}",
        report.render()
    );
    assert_eq!(
        report.failed_fixtures(),
        sorted(EXPECTED_FAILED),
        "failed set drifted:\n{}",
        report.render()
    );
    assert_eq!(
        report.cancelled_fixtures(),
        sorted(EXPECTED_CANCELLED),
        "cancellation must be empty for this corpus:\n{}",
        report.render()
    );
    assert_eq!(
        report.excluded_fixtures(),
        sorted(EXCLUDED_OUT_OF_ORACLE),
        "excluded set drifted:\n{}",
        report.render()
    );

    // Timer parks pinned to their exact pending-timer identity (r2 finding 3).
    let expected_timer: Vec<(String, Vec<String>)> = EXPECTED_TIMER_PARKED
        .iter()
        .map(|(name, timer)| ((*name).to_owned(), vec![(*timer).to_owned()]))
        .collect();
    assert_eq!(
        report.timer_parked_evidence(),
        expected_timer,
        "timer-park set or pending-timer identity drifted:\n{}",
        report.render()
    );

    // The union of every bucket is EXACTLY the 76 distinct covered names.
    let mut union: BTreeSet<String> = BTreeSet::new();
    let parked_names = report.parked_timer.iter().map(|(name, _)| name);
    for name in report
        .succeeded
        .iter()
        .chain(&report.failed)
        .chain(&report.cancelled)
        .chain(parked_names)
        .chain(&report.excluded)
    {
        assert!(union.insert(name.clone()), "duplicate outcome for {name}");
    }
    let covered: BTreeSet<String> = names.into_iter().collect();
    assert_eq!(
        union, covered,
        "the outcome union is not exactly the covered corpus"
    );
    Ok(())
}

/// Sorts a static name list into an owned, comparable vector.
fn sorted(names: &[&str]) -> Vec<String> {
    let mut owned: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    owned.sort();
    owned
}
