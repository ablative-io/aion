//! Deliverable 2 + 5: the differential over the covered ratchet. Every fixture
//! both backends accept runs through a real engine; the normalized durable
//! trails must be byte-identical, and the set of out-of-intersection refusals
//! must match a pinned list so intersection shrinkage is loud.

use crate::driver::run_differential;
use crate::fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The covered ratchet is exactly the lowering set, so no covered fixture may
/// be out-of-intersection: the pinned expected refusal list is EMPTY. A
/// covered fixture that starts refusing (at lower, emit, or select) fails this
/// pin loudly rather than silently shrinking the differential.
const EXPECTED_REFUSALS: &[&str] = &[];

/// Flagship de-risk: the first AWL workflow, end to end through both backends,
/// must produce a byte-identical durable trail. Kept as a focused single-fixture
/// proof separate from the full fan-out.
#[tokio::test(flavor = "multi_thread")]
async fn flagship_awl_hello_matches() -> TestResult {
    let names = vec![String::from("flagship/valid/awl_hello")];
    let report = run_differential(&names, "flagship").await?;
    assert!(
        report.divergences.is_empty(),
        "flagship diverged:\n{}",
        report.render()
    );
    assert!(
        report.refusals.is_empty(),
        "flagship unexpectedly refused: {:?}",
        report.refused_fixtures()
    );
    assert_eq!(
        report.identical.len(),
        1,
        "flagship did not compare an identical trail:\n{}",
        report.render()
    );
    Ok(())
}

/// The full covered differential: build both backends for every covered
/// fixture (ONE `gleam build`), run both, and assert byte-identical normalized
/// trails with no divergence, and refusals exactly the pinned set.
#[tokio::test(flavor = "multi_thread")]
async fn covered_set_is_byte_identical() -> TestResult {
    let names = fixtures::covered_paths()?;
    assert!(
        names.len() >= 60,
        "covered ratchet unexpectedly small ({}) — parse drifted",
        names.len()
    );
    let report = run_differential(&names, "covered").await?;

    let expected: Vec<String> = EXPECTED_REFUSALS
        .iter()
        .map(|entry| (*entry).to_owned())
        .collect();
    assert_eq!(
        report.refused_fixtures(),
        expected,
        "covered refusal set drifted (intersection shrinkage):\n{}",
        report.render()
    );
    assert!(
        report.divergences.is_empty(),
        "covered differential found divergences:\n{}",
        report.render()
    );
    // Every covered fixture must land in EXACTLY one outcome bucket — identical,
    // unsettled-but-identical, diverged, or refused — so none is silently
    // dropped and none is double-counted.
    let accounted = report.identical.len()
        + report.unsettled.len()
        + report.diverged.len()
        + report.refusals.len();
    assert_eq!(
        accounted,
        names.len(),
        "some covered fixture produced no outcome:\n{}",
        report.render()
    );
    println!("{}", report.render());
    Ok(())
}
