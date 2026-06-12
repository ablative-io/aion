//! Wire compatibility against the Gleam codecs — the load-bearing test.
//!
//! Every literal below is hand-derived from the codec source in
//! `../../src/stacked_dev/codecs_core.gleam` and
//! `../../src/stacked_dev/codecs_flow.gleam` (each case names the codec
//! function it mirrors). Both sides emit compact JSON in field-declaration
//! order, so each value must round-trip to the literal **byte for byte**:
//! any field-name, tag-string, or field-order drift fails here.

use std::error::Error;
use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use stacked_dev_worker::types::{
    BuildWarm, CheckResult, CheckVerdict, DevInput, DevResult, GateInput, GateResult, GateScope,
    GateVerdict, Isolation, LandInput, Landed, Placement, ProvisionInput, ResumeInput, ReviewAck,
    ReviewRequest, ScopedInput, StartupResult, StartupTask, Workspace,
};

type TestResult = Result<(), Box<dyn Error>>;

/// Decode the literal and require equality, then encode the value and
/// require the exact literal back.
fn assert_wire<T>(literal: &str, expected: &T) -> TestResult
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let decoded: T = serde_json::from_str(literal)
        .map_err(|error| format!("failed to decode {literal}: {error}"))?;
    assert_eq!(&decoded, expected, "decode mismatch for {literal}");
    let encoded = serde_json::to_string(expected)?;
    assert_eq!(encoded, literal, "encode drift from the Gleam codec shape");
    Ok(())
}

/// The workspace used across composite literals; shape from
/// `codecs_core.workspace_to_json`.
fn workspace() -> (String, Workspace) {
    let literal = r#"{"path":"/abs/repo/.yggdrasil-worktrees/stacked-dev-brief-7","branch":"stacked-dev-brief-7","placement":"local","isolation":"worktree"}"#;
    let value = Workspace {
        path: "/abs/repo/.yggdrasil-worktrees/stacked-dev-brief-7".to_owned(),
        branch: "stacked-dev-brief-7".to_owned(),
        placement: Placement::Local,
        isolation: Isolation::Worktree,
    };
    (literal.to_owned(), value)
}

/// The dev result used across composite literals; shape from
/// `codecs_core.dev_result_to_json`.
fn dev_result() -> (String, DevResult) {
    let literal = r#"{"session_id":"stacked-dev-brief-7","files_touched":["crates/aion-core/src/lib.rs"],"summary":"implemented the brief"}"#;
    let value = DevResult {
        session_id: "stacked-dev-brief-7".to_owned(),
        files_touched: vec!["crates/aion-core/src/lib.rs".to_owned()],
        summary: "implemented the brief".to_owned(),
    };
    (literal.to_owned(), value)
}

// Mirrors `codecs_core.provision_input_codec`.
#[test]
fn provision_input_wire_shape() -> TestResult {
    assert_wire(
        r#"{"repo_root":"/abs/repo","brief_id":"brief-7","base_ref":"main","placement":"local","isolation":"worktree"}"#,
        &ProvisionInput {
            repo_root: "/abs/repo".to_owned(),
            brief_id: "brief-7".to_owned(),
            base_ref: "main".to_owned(),
            placement: Placement::Local,
            isolation: Isolation::Worktree,
        },
    )
}

// Mirrors `codecs_core.workspace_codec`.
#[test]
fn workspace_wire_shape() -> TestResult {
    let (literal, value) = workspace();
    assert_wire(&literal, &value)
}

// Mirrors `codecs_core.placement_to_string` / `isolation_to_string` for
// every enum variant (the decoder accepts exactly these strings).
#[test]
fn placement_and_isolation_enum_strings() -> TestResult {
    for (placement_literal, placement) in
        [("local", Placement::Local), ("remote", Placement::Remote)]
    {
        for (isolation_literal, isolation) in [
            ("worktree", Isolation::Worktree),
            ("copy", Isolation::Copy),
            ("overlay", Isolation::Overlay),
            ("vm", Isolation::Vm),
        ] {
            assert_wire(
                &format!(
                    r#"{{"path":"/w","branch":"b","placement":"{placement_literal}","isolation":"{isolation_literal}"}}"#
                ),
                &Workspace {
                    path: "/w".to_owned(),
                    branch: "b".to_owned(),
                    placement,
                    isolation,
                },
            )?;
        }
    }
    Ok(())
}

// Mirrors `codecs_core.build_warm_to_json` / `build_warm_decoder`.
#[test]
fn build_warm_wire_shape() -> TestResult {
    assert_wire(
        r#"{"ok":false,"duration_ms":1500}"#,
        &BuildWarm {
            ok: false,
            duration_ms: 1500,
        },
    )
}

// Mirrors `codecs_core.dev_result_codec`.
#[test]
fn dev_result_wire_shape() -> TestResult {
    let (literal, value) = dev_result();
    assert_wire(&literal, &value)
}

// Mirrors `codecs_core.startup_task_codec`, `warm_build` variant: the tagged
// envelope shared by the warm_build/dev `workflow.all` fan-out.
#[test]
fn startup_task_warm_build_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &format!(r#"{{"task":"warm_build","workspace":{workspace_literal}}}"#),
        &StartupTask::WarmBuild { workspace },
    )
}

// Mirrors `codecs_core.startup_task_codec`, `dev` variant (embedding
// `codecs_core.dev_input_to_json`).
#[test]
fn startup_task_dev_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &format!(
            r#"{{"task":"dev","dev_input":{{"workspace":{workspace_literal},"brief":"Implement the widget","design":"docs/design.md","checklist":"docs/checklist.md","stories":["story-1","story-2"]}}}}"#
        ),
        &StartupTask::Dev {
            dev_input: DevInput {
                workspace,
                brief: "Implement the widget".to_owned(),
                design: "docs/design.md".to_owned(),
                checklist: "docs/checklist.md".to_owned(),
                stories: vec!["story-1".to_owned(), "story-2".to_owned()],
            },
        },
    )
}

// Mirrors `codecs_core.startup_result_codec`, `warm_build` variant.
#[test]
fn startup_result_warmed_wire_shape() -> TestResult {
    assert_wire(
        r#"{"task":"warm_build","build_warm":{"ok":true,"duration_ms":42}}"#,
        &StartupResult::Warmed {
            build_warm: BuildWarm {
                ok: true,
                duration_ms: 42,
            },
        },
    )
}

// Mirrors `codecs_core.startup_result_codec`, `dev` variant.
#[test]
fn startup_result_developed_wire_shape() -> TestResult {
    let (dev_result_literal, dev_result) = dev_result();
    assert_wire(
        &format!(r#"{{"task":"dev","dev_result":{dev_result_literal}}}"#),
        &StartupResult::Developed { dev_result },
    )
}

// Mirrors `codecs_core.scoped_input_codec`.
#[test]
fn scoped_input_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"files_touched":["crates/aion-core/src/lib.rs"]}}"#
        ),
        &ScopedInput {
            workspace,
            files_touched: vec!["crates/aion-core/src/lib.rs".to_owned()],
        },
    )
}

// Mirrors `codecs_core.check_result_codec` with `check_verdict_to_json`'s
// pass shape.
#[test]
fn check_result_pass_wire_shape() -> TestResult {
    assert_wire(
        r#"{"verdict":{"outcome":"pass"},"affected_modules":["aion-core"],"checked_scope":"affected: aion-core"}"#,
        &CheckResult {
            verdict: CheckVerdict::Pass,
            affected_modules: vec!["aion-core".to_owned()],
            checked_scope: "affected: aion-core".to_owned(),
        },
    )
}

// Mirrors `codecs_core.check_result_codec` with the fail verdict and the
// loud workspace-wide fallback scope string from `locals.scoped_checks`.
#[test]
fn check_result_fail_wire_shape() -> TestResult {
    assert_wire(
        r#"{"verdict":{"outcome":"fail","diagnostics":"error: unused variable"},"affected_modules":[],"checked_scope":"workspace-wide fallback: affected scoping returned an empty set"}"#,
        &CheckResult {
            verdict: CheckVerdict::Fail {
                diagnostics: "error: unused variable".to_owned(),
            },
            affected_modules: Vec::new(),
            checked_scope: "workspace-wide fallback: affected scoping returned an empty set"
                .to_owned(),
        },
    )
}

// Mirrors `codecs_core.resume_input_codec`.
#[test]
fn resume_input_wire_shape() -> TestResult {
    assert_wire(
        r#"{"session_id":"stacked-dev-brief-7","feedback":"error: unused variable"}"#,
        &ResumeInput {
            session_id: "stacked-dev-brief-7".to_owned(),
            feedback: "error: unused variable".to_owned(),
        },
    )
}

// Mirrors `codecs_flow.gate_input_codec` with `gate_scope_to_json`'s
// workspace_wide shape.
#[test]
fn gate_input_workspace_wide_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"files_touched":["crates/aion-core/src/lib.rs"],"scope":{{"kind":"workspace_wide"}}}}"#
        ),
        &GateInput {
            workspace,
            files_touched: vec!["crates/aion-core/src/lib.rs".to_owned()],
            scope: GateScope::WorkspaceWide,
        },
    )
}

// Mirrors `codecs_flow.gate_input_codec` with `gate_scope_to_json`'s
// affected_closure shape (the typed seam).
#[test]
fn gate_input_affected_closure_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"files_touched":[],"scope":{{"kind":"affected_closure","modules":["aion-core"]}}}}"#
        ),
        &GateInput {
            workspace,
            files_touched: Vec::new(),
            scope: GateScope::AffectedClosure {
                modules: vec!["aion-core".to_owned()],
            },
        },
    )
}

// Mirrors `codecs_flow.gate_result_codec` with `gate_verdict_to_json`'s pass
// shape.
#[test]
fn gate_result_pass_wire_shape() -> TestResult {
    assert_wire(
        r#"{"verdict":{"outcome":"pass"}}"#,
        &GateResult {
            verdict: GateVerdict::Pass,
        },
    )
}

// Mirrors `codecs_flow.gate_result_codec` with the fail verdict.
#[test]
fn gate_result_fail_wire_shape() -> TestResult {
    assert_wire(
        r#"{"verdict":{"outcome":"fail","report":"error: cross-crate lint failure"}}"#,
        &GateResult {
            verdict: GateVerdict::Fail {
                report: "error: cross-crate lint failure".to_owned(),
            },
        },
    )
}

// Mirrors `codecs_flow.review_request_codec`.
#[test]
fn review_request_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let (dev_result_literal, dev_result) = dev_result();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"brief_id":"brief-7","reviewers":["sample-reviewer"],"dev_result":{dev_result_literal},"gate_result":{{"verdict":{{"outcome":"pass"}}}}}}"#
        ),
        &ReviewRequest {
            workspace,
            brief_id: "brief-7".to_owned(),
            reviewers: vec!["sample-reviewer".to_owned()],
            dev_result,
            gate_result: GateResult {
                verdict: GateVerdict::Pass,
            },
        },
    )
}

// Mirrors `codecs_flow.review_ack_codec`.
#[test]
fn review_ack_wire_shape() -> TestResult {
    assert_wire(
        r#"{"request_id":"rev-1"}"#,
        &ReviewAck {
            request_id: "rev-1".to_owned(),
        },
    )
}

// Mirrors `codecs_flow.land_input_codec`.
#[test]
fn land_input_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let (dev_result_literal, dev_result) = dev_result();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"base_ref":"main","dev_result":{dev_result_literal}}}"#
        ),
        &LandInput {
            workspace,
            base_ref: "main".to_owned(),
            dev_result,
        },
    )
}

// Mirrors `codecs_flow.landed_codec`.
#[test]
fn landed_wire_shape() -> TestResult {
    assert_wire(
        r#"{"branch":"stacked-dev-brief-7","merged_into":"main"}"#,
        &Landed {
            branch: "stacked-dev-brief-7".to_owned(),
            merged_into: "main".to_owned(),
        },
    )
}
