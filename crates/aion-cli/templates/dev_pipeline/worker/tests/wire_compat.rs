//! Wire compatibility against the Gleam codecs — the load-bearing test.
//!
//! Every literal below is hand-derived from the codec source: the
//! schema-generated module `../../src/{{name}}_io.gleam` for the gate
//! payloads, and the hand-written `../../src/{{name}}/codecs_core.gleam`
//! / `codecs_flow.gleam` for the activity payloads (each case names the
//! codec it mirrors). Both sides emit compact JSON in field-declaration
//! order, so each value must round-trip to the literal **byte for byte**:
//! any field-name, tag-string, or field-order drift fails here.

use std::error::Error;
use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;

// The scaffolded crate's own library, in its own import group so the
// statement order holds for any project name.
use {{name}}_worker::types::{
    BuildWarm, CheckResult, CheckVerdict, DevInput, DevResult, GateInput, GateResult, GateScope,
    GateVerdict, Isolation, LandInput, Landed, Placement, ProvisionInput, ResumeInput, ReviewAck,
    ReviewRequest, ScopedInput, StartupResult, StartupTask, Workspace,
};

type TestResult = Result<(), Box<dyn Error>>;

/// Wrap JSON member text in an object literal. Spelled as a helper rather
/// than `format!` brace escapes because this file doubles as an `aion new`
/// template, whose renderer treats a doubled opening brace as an unresolved
/// placeholder.
fn json_object(members: &str) -> String {
    format!("{}{members}{}", '{', '}')
}

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
    let literal = r#"{"path":"/abs/repo/.yggdrasil-worktrees/{{name}}-brief-7","branch":"{{name}}-brief-7","placement":"local","isolation":"worktree"}"#;
    let value = Workspace {
        path: "/abs/repo/.yggdrasil-worktrees/{{name}}-brief-7".to_owned(),
        branch: "{{name}}-brief-7".to_owned(),
        placement: Placement::Local,
        isolation: Isolation::Worktree,
    };
    (literal.to_owned(), value)
}

/// The dev result used across composite literals; shape from
/// `codecs_core.dev_result_to_json`.
fn dev_result() -> (String, DevResult) {
    let literal = r#"{"session_id":"{{name}}-brief-7","files_touched":["crates/aion-core/src/lib.rs"],"summary":"implemented the brief"}"#;
    let value = DevResult {
        session_id: "{{name}}-brief-7".to_owned(),
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
                &json_object(&format!(
                    r#""path":"/w","branch":"b","placement":"{placement_literal}","isolation":"{isolation_literal}""#
                )),
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
        &json_object(&format!(
            r#""task":"warm_build","workspace":{workspace_literal}"#
        )),
        &StartupTask::WarmBuild { workspace },
    )
}

// Mirrors `codecs_core.startup_task_codec`, `dev` variant (embedding
// `codecs_core.dev_input_to_json`).
#[test]
fn startup_task_dev_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let dev_input_literal = json_object(&format!(
        r#""workspace":{workspace_literal},"brief":"Implement the widget","design":"docs/design.md","checklist":"docs/checklist.md","stories":["story-1","story-2"]"#
    ));
    assert_wire(
        &json_object(&format!(r#""task":"dev","dev_input":{dev_input_literal}"#)),
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
        &json_object(&format!(
            r#""task":"dev","dev_result":{dev_result_literal}"#
        )),
        &StartupResult::Developed { dev_result },
    )
}

// Mirrors `codecs_core.scoped_input_codec`.
#[test]
fn scoped_input_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &json_object(&format!(
            r#""workspace":{workspace_literal},"files_touched":["crates/aion-core/src/lib.rs"]"#
        )),
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
        r#"{"session_id":"{{name}}-brief-7","feedback":"error: unused variable"}"#,
        &ResumeInput {
            session_id: "{{name}}-brief-7".to_owned(),
            feedback: "error: unused variable".to_owned(),
        },
    )
}

// Mirrors the generated `gate_input_to_json` / `gate_input_scope_to_json`
// (`{{name}}_io`) workspace_wide shape: the `modules` field is omitted
// when absent.
#[test]
fn gate_input_workspace_wide_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let scope_literal = json_object(r#""kind":"workspace_wide""#);
    assert_wire(
        &json_object(&format!(
            r#""workspace":{workspace_literal},"files_touched":["crates/aion-core/src/lib.rs"],"scope":{scope_literal}"#
        )),
        &GateInput {
            workspace,
            files_touched: vec!["crates/aion-core/src/lib.rs".to_owned()],
            scope: GateScope::WorkspaceWide,
        },
    )
}

// Mirrors the generated `gate_input_scope_to_json` affected_closure shape
// (the typed seam).
#[test]
fn gate_input_affected_closure_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let scope_literal = json_object(r#""kind":"affected_closure","modules":["aion-core"]"#);
    assert_wire(
        &json_object(&format!(
            r#""workspace":{workspace_literal},"files_touched":[],"scope":{scope_literal}"#
        )),
        &GateInput {
            workspace,
            files_touched: Vec::new(),
            scope: GateScope::AffectedClosure {
                modules: vec!["aion-core".to_owned()],
            },
        },
    )
}

// Mirrors the generated `gate_output_to_json` pass shape: the `report`
// field is omitted when absent.
#[test]
fn gate_result_pass_wire_shape() -> TestResult {
    assert_wire(
        r#"{"verdict":{"outcome":"pass"}}"#,
        &GateResult {
            verdict: GateVerdict::Pass,
        },
    )
}

// Mirrors the generated `gate_output_to_json` fail shape.
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
    let verdict_literal = json_object(r#""outcome":"pass""#);
    let gate_result_literal = json_object(&format!(r#""verdict":{verdict_literal}"#));
    assert_wire(
        &json_object(&format!(
            r#""workspace":{workspace_literal},"brief_id":"brief-7","reviewers":["sample-reviewer"],"dev_result":{dev_result_literal},"gate_result":{gate_result_literal}"#
        )),
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
        &json_object(&format!(
            r#""workspace":{workspace_literal},"base_ref":"main","dev_result":{dev_result_literal}"#
        )),
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
        r#"{"branch":"{{name}}-brief-7","merged_into":"main"}"#,
        &Landed {
            branch: "{{name}}-brief-7".to_owned(),
            merged_into: "main".to_owned(),
        },
    )
}
