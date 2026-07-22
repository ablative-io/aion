//! r2 finding 6: establish the CAUSE of the four child-fixture failures, not
//! just their terminal kind.
//!
//! Declared `child` workflows are launched through `workflow.spawn`, never
//! through the `ActivityDispatcher` (the emitter's `CHILD_WITNESS` is a type
//! anchor the engine never calls). Their synthesized workflow type is not
//! registered for spawning in the differential's package, so the parent fails
//! at the child boundary with an `AwlChildFailed` error whose message reports
//! `child_workflow_type_not_loaded:<child>`. This test asserts that specific
//! boundary evidence, so an unrelated parent input/activity-decode failure
//! (a different tag, a different message) cannot satisfy the pin.

use aion_core::Event;

use crate::driver::build_spliced;
use crate::run::{Disposition, run_package};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The covered child-spawning fixtures that fail AT the child boundary
/// (`AwlChildFailed`). `declarations_combined` is deliberately NOT here: with
/// minimal inputs it takes a data-driven `route failure` (`AwlOutcomeFailure`)
/// before reaching its child, so it is a plain error-path fixture, not a
/// child-boundary one — proof that this test pins the CAUSE, not the family.
const CHILD_FIXTURES: &[&str] = &[
    "declarations/valid/child_call_awaited",
    "declarations/valid/child_spawn_combo",
    "declarations/valid/spawn_detached",
];

/// Each child fixture fails with an `AwlChildFailed` error reporting
/// `child_workflow_type_not_loaded` — the precise child-execution boundary,
/// before the terminal `WorkflowFailed`.
#[tokio::test(flavor = "multi_thread")]
async fn child_spawning_fixtures_fail_at_the_child_boundary() -> TestResult {
    let names: Vec<String> = CHILD_FIXTURES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let fixtures = build_spliced(&names, "child_boundary").await?;
    assert_eq!(
        fixtures.len(),
        CHILD_FIXTURES.len(),
        "every child fixture must build both backends"
    );

    for fixture in &fixtures {
        let outcome = run_package(
            fixture.direct_package.clone(),
            &fixture.entry_module,
            &fixture.input,
            fixture.action_results.clone(),
            &fixture.entry_bytes,
        )
        .await?;
        let module = fixture.entry_module.as_str();
        assert_eq!(
            outcome.disposition,
            Disposition::Failed,
            "{module} must fail at the child boundary"
        );
        let details = child_failure_details(&outcome.trail)
            .ok_or_else(|| format!("{module}: no WorkflowFailed details recorded"))?;
        // The `AwlChildFailed` tag is the PRIMARY child-boundary evidence: it
        // distinguishes a child-spawn failure from a parent decode failure
        // (`AwlDecodeInputFailed`) or a data-driven route-failure
        // (`AwlOutcomeFailure`), so an unrelated parent failure cannot satisfy it.
        assert_eq!(
            details.get("tag").and_then(serde_json::Value::as_str),
            Some("AwlChildFailed"),
            "{module}: the failure must be an AwlChildFailed at the child boundary, got {details}"
        );
        // The message names the child-spawn boundary — either the unloaded child
        // type (awaited: `child_workflow_type_not_loaded:<child>`) or the detached
        // spawn (`detached spawn failed`).
        let message = details
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains("child_workflow_type_not_loaded") || message.contains("spawn"),
            "{module}: the child-boundary message must name the child-spawn boundary, got `{message}`"
        );
    }
    Ok(())
}

/// The decoded `WorkflowFailed.error.details` JSON object, if present.
fn child_failure_details(trail: &[Event]) -> Option<serde_json::Value> {
    trail.iter().find_map(|event| match event {
        Event::WorkflowFailed { error, .. } => error
            .details
            .as_ref()
            .and_then(|payload| serde_json::from_slice(payload.bytes()).ok()),
        _ => None,
    })
}
