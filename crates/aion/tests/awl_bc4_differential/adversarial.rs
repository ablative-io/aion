//! Deliverable 4: the eight rev-2 adversarial fixtures (all net-new) and their
//! honest classification (decision 16 — coverage is never forced).
//!
//! Six lower AND emit, so they join the `covered.rs` ratchet and run through
//! the differential to byte-identical trails:
//! - `empty_fork_collection`  — a fork over a possibly-empty input collection.
//! - `runtime_sized_fork`     — a fork whose width is only known at run time.
//! - `timeout_inside_retry`   — a per-attempt timeout nested in a retry schedule.
//! - `unicode_payloads`       — UTF-8 string literals + concat onto the wire.
//! - `child_spawn_combo`      — an awaited child plus a detached `spawn`.
//! - `max_arity_record`       — a high-arity record (wide tuple).
//!
//! Two are out-of-intersection and refuse (recorded, never failed):
//! - `nested_handlers`   — the reference emits its `on failure` region, but the
//!   direct path does not yet lower `on failure` (pinned in
//!   `aion-awl/src/mir/deferred_tests.rs`).
//! - `zero_step_workflow` — below BOTH backends' floor (no steps to execute);
//!   it lives outside any `valid/` sweep dir for that reason.

use aion_awl::mir::lower;

use crate::driver::run_differential;
use crate::fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The six adversarial fixtures both backends accept.
const LOWERING: &[&str] = &[
    "dag-fork/valid/empty_fork_collection",
    "dag-fork/valid/runtime_sized_fork",
    "declarations/valid/timeout_inside_retry",
    "step-bodies/valid/unicode_payloads",
    "declarations/valid/child_spawn_combo",
    "header-types/valid/max_arity_record",
];

/// The two out-of-intersection adversarial fixtures and the substring each
/// direct-path refusal must contain.
const REFUSING: &[(&str, &str)] = &[
    ("loop-outcomes/valid/nested_handlers", "on failure"),
    ("header-types/zero_step_workflow", "no start region"),
];

/// Classification pin: the six lower, the two refuse with their expected
/// refusal text. A drift in either direction (a lowering fixture starting to
/// refuse, or a refusing one starting to lower) fails here.
#[test]
fn adversarial_fixtures_classify_as_expected() -> TestResult {
    for name in LOWERING {
        let loaded = fixtures::load(name)?;
        lower(&loaded.document, Some(loaded.dir.as_path()))
            .map_err(|error| format!("{name} must lower but refused: {error}"))?;
    }
    for (name, expected) in REFUSING {
        let loaded = fixtures::load(name)?;
        match lower(&loaded.document, Some(loaded.dir.as_path())) {
            Err(error) => {
                let text = error.to_string();
                assert!(
                    text.contains(expected),
                    "{name} refusal must contain `{expected}`, got `{text}`"
                );
            }
            Ok(_) => return Err(format!("{name} must refuse (`{expected}`) but lowered").into()),
        }
    }
    Ok(())
}

/// The differential over all eight adversarial fixtures: the six lowering ones
/// produce byte-identical normalized trails across both backends; the two
/// refusals are recorded out-of-intersection (exactly the pinned set), never a
/// divergence.
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_differential_is_byte_identical() -> TestResult {
    let mut names: Vec<String> = LOWERING.iter().map(|entry| (*entry).to_owned()).collect();
    names.extend(REFUSING.iter().map(|(name, _)| (*name).to_owned()));
    let report = run_differential(&names, "adversarial").await?;

    let mut expected_refusals: Vec<String> = REFUSING
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    expected_refusals.sort();
    assert_eq!(
        report.refused_fixtures(),
        expected_refusals,
        "adversarial refusal set drifted:\n{}",
        report.render()
    );
    assert!(
        report.divergences.is_empty(),
        "adversarial differential found divergences:\n{}",
        report.render()
    );
    let compared = report.identical.len() + report.unsettled.len();
    assert_eq!(
        compared,
        LOWERING.len(),
        "every lowering adversarial fixture must produce a compared trail:\n{}",
        report.render()
    );
    println!("{}", report.render());
    Ok(())
}
