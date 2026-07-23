//! The oracle self-test (r2 finding 1): a mutation test proving the harness
//! cannot compare a package to itself.
//!
//! `run_package` is bound to the exact entry bytes it expects to load. This
//! test deliberately hands it the REFERENCE package while claiming the DIRECT
//! `select()` bytes as the expectation — the exact mis-binding a regression
//! (running `reference.clone()` as the direct side) would introduce. The run
//! MUST be refused before execution. A passing differential is therefore proof
//! the direct engine loaded the direct production, not the reference twice.

use crate::driver::build_spliced;
use crate::run::run_package;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Running the reference package under the DIRECT entry-byte expectation must
/// fail the splice binding before any engine run.
#[tokio::test(flavor = "multi_thread")]
async fn reference_package_under_direct_expectation_is_refused() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let fixtures =
        build_spliced(&[String::from("flagship/valid/awl_hello")], "oracle_self").await?;
    let hello = fixtures.first().ok_or("no awl_hello spliced fixture")?;

    // The direct package under its OWN (direct) expectation runs fine — the
    // control that the binding is not simply always-failing.
    let ok = run_package(
        hello.direct_package.clone(),
        "awl_hello",
        &hello.input,
        hello.action_results.clone(),
        &hello.entry_bytes,
    )
    .await;
    assert!(
        ok.is_ok(),
        "the direct package under its own expectation must run"
    );

    // Reconstruct the reference package (splice back the reference entry) and
    // try to run it while CLAIMING the direct select() bytes — the mis-binding.
    // The reference entry beam differs from the direct bytes, so run_package
    // must refuse.
    let reference_entry = hello
        .reference_package
        .beams()
        .get(&hello.entry_module)
        .ok_or("reference package lost its entry beam")?
        .to_vec();
    assert_ne!(
        reference_entry, hello.entry_bytes,
        "precondition: the reference and direct entry bytes must differ"
    );
    let refused = run_package(
        hello.reference_package.clone(),
        "awl_hello",
        &hello.input,
        hello.action_results.clone(),
        &hello.entry_bytes,
    )
    .await;
    let error = refused.err().ok_or(
        "MUTATION ESCAPED: running the reference package under the direct \
         expectation was NOT refused — the oracle could compare a package to itself",
    )?;
    assert!(
        error.to_string().contains("splice binding violated"),
        "refusal must be the splice-binding error, got: {error}"
    );
    Ok(())
}
