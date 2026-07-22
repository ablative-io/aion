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
//! Two are out-of-intersection and refuse (recorded, never failed), pinned to
//! their EXACT `LowerError` variant, message, and span:
//! - `nested_handlers`   — the reference emits its `on failure` region, but the
//!   direct path does not yet lower it (`Unsupported { shape: "on failure" }`).
//! - `zero_step_workflow` — below BOTH backends' floor (no steps to execute);
//!   `Message("the workflow has no start region")`.

use aion_awl::Span;
use aion_awl::mir::{LowerError, lower};

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

/// The two out-of-intersection adversarial fixtures (recorded refusals).
const REFUSING: &[&str] = &[
    "loop-outcomes/valid/nested_handlers",
    "header-types/zero_step_workflow",
];

/// Classification pin: the six lower; the two refuse with their EXACT
/// `LowerError` variant, message/shape, and span (not a substring).
#[test]
fn adversarial_fixtures_classify_as_expected() -> TestResult {
    for name in LOWERING {
        let loaded = fixtures::load(name)?;
        lower(&loaded.document, Some(loaded.dir.as_path()))
            .map_err(|error| format!("{name} must lower but refused: {error}"))?;
    }

    // `nested_handlers`: reference class `Unsupported { shape: "on failure" }`
    // anchored to the guarding step `fulfil`, pinned to the COMPLETE span (kept
    // identical to the aion-awl deferred ratchet so the two copies cannot drift).
    let nested = fixtures::load("loop-outcomes/valid/nested_handlers")?;
    match lower(&nested.document, Some(nested.dir.as_path())) {
        Err(LowerError::Unsupported { shape, span }) => {
            assert_eq!(shape, "on failure");
            assert_eq!(
                span,
                Span {
                    start: 377,
                    end: 383,
                    line: 13,
                    column: 6
                }
            );
        }
        other => {
            return Err(format!("nested_handlers must refuse at `on failure`: {other:?}").into());
        }
    }

    // `zero_step_workflow`: `Message` (span-carrying) at the workflow header,
    // pinned to the COMPLETE span.
    let zero = fixtures::load("header-types/zero_step_workflow")?;
    match lower(&zero.document, Some(zero.dir.as_path())) {
        Err(LowerError::Message { message, span }) => {
            assert_eq!(message, "the workflow has no start region");
            assert_eq!(
                span,
                Span {
                    start: 0,
                    end: 189,
                    line: 1,
                    column: 1
                }
            );
        }
        other => {
            return Err(
                format!("zero_step_workflow must refuse (no start region): {other:?}").into(),
            );
        }
    }
    Ok(())
}

/// The differential over all eight adversarial fixtures: the six lowering ones
/// produce byte-identical normalized trails across both backends (each a
/// completed/failed/parked comparison, never an infra failure); the two
/// refusals are recorded out-of-intersection (exactly the pinned set).
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_differential_is_byte_identical() -> TestResult {
    let mut names: Vec<String> = LOWERING.iter().map(|entry| (*entry).to_owned()).collect();
    names.extend(REFUSING.iter().map(|name| (*name).to_owned()));
    let report = run_differential(&names, "adversarial", &[]).await?;
    println!("{}", report.render());
    println!("SUCCEEDED {:?}", report.succeeded);
    println!("FAILED {:?}", report.failed_fixtures());
    println!("TIMER-PARKED {:?}", report.timer_parked_fixtures());

    let mut expected_refusals: Vec<String> =
        REFUSING.iter().map(|name| (*name).to_owned()).collect();
    expected_refusals.sort();
    assert_eq!(
        report.refused_fixtures(),
        expected_refusals,
        "adversarial refusal set drifted:\n{}",
        report.render()
    );
    assert!(
        report.infra.is_empty(),
        "adversarial differential hit infrastructure failures:\n{}",
        report.render()
    );
    assert!(
        report.divergences.is_empty(),
        "adversarial differential found divergences:\n{}",
        report.render()
    );
    assert_eq!(
        report.identical_count(),
        LOWERING.len(),
        "every lowering adversarial fixture must produce a byte-identical comparison:\n{}",
        report.render()
    );
    Ok(())
}
