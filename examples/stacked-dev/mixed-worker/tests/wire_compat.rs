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
use stacked_dev_worker::dispatch::{
    BriefOutcome, DispatchError, DispatchInput, DispatchResult, StackedDevError,
};
use stacked_dev_worker::types::{
    AcceptanceVerdict, Alignment, AssembleInput, AssembledWave, Attestation, BriefDevInput,
    BriefDevResult, BriefDocument, BriefRequirement, BuildWarm, ChangeKind, CheckResult,
    CheckVerdict, ChecklistClaim, Claim, DevBlock, DevEnrichment, DevInput, DevReport, DevResult,
    DevStatus, EnrichInput, Enrichment, ExecutionBlock, ExecutionStatus, ExecutionVerdict,
    FileChange, GateBlock, GateInput, GateResult, GateScope, GateVerdict, Isolation, LandInput,
    Landed, Placement, ProvisionInput, RequirementFiles, ResolvedAdr, ResolvedContext,
    ResolvedItem, ResolvedProvenance, ResumeInput, ReviewAck, ReviewBlock, ReviewEnrichment,
    ReviewInput, ReviewReport, ReviewRequest, ReviewVerification, ScopedInput, ScoutBlock,
    ScoutEnrichment, ScoutInput, ScoutReport, StackedDevInput, StackedDevResult, StartupResult,
    StartupTask, StoryClaim, WaveEntry, Workspace,
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

/// The scout report used across composite literals; shape from
/// `aion_stacked_dev_io.scout_report_to_json`.
fn scout_report() -> (String, ScoutReport) {
    let literal = r#"{"summary":"scouted","enrichments":[{"id":"R1","files":["src/a.gleam"],"context":["match conventions"],"approach":"add it","notes":""}],"verification":["gleam test"]}"#;
    let value = ScoutReport {
        summary: "scouted".to_owned(),
        enrichments: vec![ScoutEnrichment {
            id: "R1".to_owned(),
            files: vec!["src/a.gleam".to_owned()],
            context: vec!["match conventions".to_owned()],
            approach: "add it".to_owned(),
            notes: String::new(),
        }],
        verification: vec!["gleam test".to_owned()],
    };
    (literal.to_owned(), value)
}

/// The dev report used across composite literals; shape from
/// `aion_stacked_dev_io.dev_report_to_json`.
fn dev_report() -> (String, DevReport) {
    let literal = r#"{"summary":"implemented","commit_message":"feat: R1","enrichments":[{"id":"R1","status":"implemented","files_changed":[{"path":"src/a.gleam","change":"modified","note":"added"}],"how":"added it","deviation":"","checklist":[{"id":"C1","done":true,"note":"done"}],"stories":[{"id":"S1","satisfied":true,"note":"ok"}]}],"attestation":{"no_panics":true,"no_unsafe":true,"boundaries_respected":true,"tests_pass":true}}"#;
    let value = DevReport {
        summary: "implemented".to_owned(),
        commit_message: "feat: R1".to_owned(),
        enrichments: vec![DevEnrichment {
            id: "R1".to_owned(),
            status: DevStatus::Implemented,
            files_changed: vec![FileChange {
                path: "src/a.gleam".to_owned(),
                change: ChangeKind::Modified,
                note: "added".to_owned(),
            }],
            how: "added it".to_owned(),
            deviation: String::new(),
            checklist: vec![ChecklistClaim {
                id: "C1".to_owned(),
                done: true,
                note: "done".to_owned(),
            }],
            stories: vec![StoryClaim {
                id: "S1".to_owned(),
                satisfied: true,
                note: "ok".to_owned(),
            }],
        }],
        attestation: Attestation {
            no_panics: Claim(true),
            no_unsafe: Claim(true),
            boundaries_respected: Claim(true),
            tests_pass: Claim(true),
        },
    };
    (literal.to_owned(), value)
}

/// The review report used across composite literals; shape from
/// `aion_stacked_dev_io.review_report_to_json`.
fn review_report() -> (String, ReviewReport) {
    let literal = r#"{"summary":"verified","commit_message":"","enrichments":[{"id":"R1","alignment":"aligned","acceptance":[{"criterion":"it exists","met":true,"evidence":"src/a.gleam:1"}],"checklist":["C1"],"stories":["S1"],"issues":[],"fixes":[]}],"verification":[{"criterion":"gleam test","passed":true,"note":""}]}"#;
    let value = ReviewReport {
        summary: "verified".to_owned(),
        commit_message: String::new(),
        enrichments: vec![ReviewEnrichment {
            id: "R1".to_owned(),
            alignment: Alignment::Aligned,
            acceptance: vec![AcceptanceVerdict {
                criterion: "it exists".to_owned(),
                met: true,
                evidence: "src/a.gleam:1".to_owned(),
            }],
            checklist: vec!["C1".to_owned()],
            stories: vec!["S1".to_owned()],
            issues: Vec::new(),
            fixes: Vec::new(),
        }],
        verification: vec![ReviewVerification {
            criterion: "gleam test".to_owned(),
            passed: true,
            note: String::new(),
        }],
    };
    (literal.to_owned(), value)
}

/// A two-requirement brief document carrying scout/dev/review blocks on its
/// first requirement and an execution block; shape from
/// `codecs_brief.brief_document_to_json`.
fn brief_document() -> (String, BriefDocument) {
    let (execution_literal, execution) = execution_block();
    let literal = format!(
        r#"{{"id":"BD-009","cluster":"brief-dev","title":"t","depends_on":[],"blocked_by":[],"checklist":["C1"],"stories":["S1"],"design_anchor":["ADR-008"],"purpose":"p","task":"k","requirements":[{{"id":"R1","title":"rt","spec":"rs","acceptance":["a"],"files":{{"create":[],"modify":["src/a.gleam"],"delete":[]}},"checklist":["C1"],"stories":["S1"],"scout":{{"files":["src/a.gleam"],"context":["c"],"approach":"ap","notes":""}},"dev":{{"status":"implemented","files_changed":[{{"path":"src/a.gleam","change":"modified","note":"n"}}],"how":"h","deviation":"","checklist":[{{"id":"C1","done":true,"note":""}}],"stories":[{{"id":"S1","satisfied":true,"note":""}}]}},"review":{{"alignment":"aligned","acceptance":[{{"criterion":"a","met":true,"evidence":"e"}}],"checklist":["C1"],"stories":["S1"],"issues":[],"fixes":[]}}}},{{"id":"R2","title":"rt2","spec":"rs2","acceptance":["a2"],"files":{{"create":[],"modify":[],"delete":[]}},"checklist":[],"stories":[]}}],"boundaries":["b"],"verification":["v"],"execution":{execution_literal}}}"#
    );
    let value = BriefDocument {
        id: "BD-009".to_owned(),
        cluster: "brief-dev".to_owned(),
        title: "t".to_owned(),
        depends_on: Vec::new(),
        blocked_by: Vec::new(),
        checklist: vec!["C1".to_owned()],
        stories: vec!["S1".to_owned()],
        design_anchor: vec!["ADR-008".to_owned()],
        purpose: "p".to_owned(),
        task: "k".to_owned(),
        requirements: vec![
            BriefRequirement {
                id: "R1".to_owned(),
                title: "rt".to_owned(),
                spec: "rs".to_owned(),
                acceptance: vec!["a".to_owned()],
                files: RequirementFiles {
                    create: Vec::new(),
                    modify: vec!["src/a.gleam".to_owned()],
                    delete: Vec::new(),
                },
                checklist: vec!["C1".to_owned()],
                stories: vec!["S1".to_owned()],
                scout: Some(ScoutBlock {
                    files: vec!["src/a.gleam".to_owned()],
                    context: vec!["c".to_owned()],
                    approach: "ap".to_owned(),
                    notes: String::new(),
                }),
                dev: Some(DevBlock {
                    status: DevStatus::Implemented,
                    files_changed: vec![FileChange {
                        path: "src/a.gleam".to_owned(),
                        change: ChangeKind::Modified,
                        note: "n".to_owned(),
                    }],
                    how: "h".to_owned(),
                    deviation: String::new(),
                    checklist: vec![ChecklistClaim {
                        id: "C1".to_owned(),
                        done: true,
                        note: String::new(),
                    }],
                    stories: vec![StoryClaim {
                        id: "S1".to_owned(),
                        satisfied: true,
                        note: String::new(),
                    }],
                }),
                review: Some(ReviewBlock {
                    alignment: Alignment::Aligned,
                    acceptance: vec![AcceptanceVerdict {
                        criterion: "a".to_owned(),
                        met: true,
                        evidence: "e".to_owned(),
                    }],
                    checklist: vec!["C1".to_owned()],
                    stories: vec!["S1".to_owned()],
                    issues: Vec::new(),
                    fixes: Vec::new(),
                }),
            },
            BriefRequirement {
                id: "R2".to_owned(),
                title: "rt2".to_owned(),
                spec: "rs2".to_owned(),
                acceptance: vec!["a2".to_owned()],
                files: RequirementFiles {
                    create: Vec::new(),
                    modify: Vec::new(),
                    delete: Vec::new(),
                },
                checklist: Vec::new(),
                stories: Vec::new(),
                scout: None,
                dev: None,
                review: None,
            },
        ],
        boundaries: vec!["b".to_owned()],
        verification: vec!["v".to_owned()],
        execution: Some(execution),
    };
    (literal, value)
}

/// The execution block used inside the brief document and the enrich pin;
/// shape from `codecs_brief_blocks.execution_block_to_json`. The gate and
/// attestation are distinct objects (P1).
fn execution_block() -> (String, ExecutionBlock) {
    let literal = r#"{"status":"landed","workflow_id":"wf","branch":"stacked-dev-BD-009","session_id":"stacked-dev-BD-009","gate":{"fmt":true,"clippy":true,"tests":true,"fix_rounds":0},"attestation":{"no_panics":true,"no_unsafe":true,"boundaries_respected":true,"tests_pass":true},"review_verdict":"approved","landed_commit":"","merged_into":"main","completed_at":"123"}"#;
    let value = ExecutionBlock {
        status: ExecutionStatus::Landed,
        workflow_id: "wf".to_owned(),
        branch: "stacked-dev-BD-009".to_owned(),
        session_id: "stacked-dev-BD-009".to_owned(),
        gate: GateBlock {
            fmt: true,
            clippy: true,
            tests: true,
            fix_rounds: 0,
        },
        attestation: Attestation {
            no_panics: Claim(true),
            no_unsafe: Claim(true),
            boundaries_respected: Claim(true),
            tests_pass: Claim(true),
        },
        review_verdict: ExecutionVerdict::Approved,
        landed_commit: String::new(),
        merged_into: "main".to_owned(),
        completed_at: "123".to_owned(),
    };
    (literal.to_owned(), value)
}

/// The resolved context used across composite literals; shape from
/// `codecs_brief.resolved_context_to_json`.
fn resolved_context() -> (String, ResolvedContext) {
    let literal = r#"{"adrs":[{"id":"ADR-008","title":"replace","decision":"d","quote":"q","decided_by":"Tom"}],"checklist":[{"id":"C1","text":"ct"}],"stories":[{"id":"S1","text":"st"}],"constraints":[{"id":"CN1","text":"nt"}],"intention":"i","design_path":"docs/design/brief-dev/design.json","provenance":{"requested_by":"Tom","quote":"do this"}}"#;
    let value = ResolvedContext {
        adrs: vec![ResolvedAdr {
            id: "ADR-008".to_owned(),
            title: "replace".to_owned(),
            decision: "d".to_owned(),
            quote: "q".to_owned(),
            decided_by: "Tom".to_owned(),
        }],
        checklist: vec![ResolvedItem {
            id: "C1".to_owned(),
            text: "ct".to_owned(),
        }],
        stories: vec![ResolvedItem {
            id: "S1".to_owned(),
            text: "st".to_owned(),
        }],
        constraints: vec![ResolvedItem {
            id: "CN1".to_owned(),
            text: "nt".to_owned(),
        }],
        intention: "i".to_owned(),
        design_path: "docs/design/brief-dev/design.json".to_owned(),
        provenance: ResolvedProvenance {
            requested_by: "Tom".to_owned(),
            quote: "do this".to_owned(),
        },
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

// Mirrors `codecs_core.startup_task_codec`, `dev` variant (embedding the
// reshaped `codecs_core.dev_input_to_json` — workspace + projected prompt).
#[test]
fn startup_task_dev_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &format!(
            r#"{{"task":"dev","dev_input":{{"workspace":{workspace_literal},"prompt":"Implement the brief..."}}}}"#
        ),
        &StartupTask::Dev {
            dev_input: DevInput {
                workspace,
                prompt: "Implement the brief...".to_owned(),
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

// Mirrors `codecs_core.startup_result_codec`, `dev` variant — now carrying the
// dev report (BD-003).
#[test]
fn startup_result_developed_wire_shape() -> TestResult {
    let (dev_report_literal, dev_report) = dev_report();
    assert_wire(
        &format!(r#"{{"task":"dev","dev_report":{dev_report_literal}}}"#),
        &StartupResult::Developed { dev_report },
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
        r#"{"session_id":"stacked-dev-brief-7","feedback":"error: unused variable","workspace_path":"/repo/.yggdrasil-worktrees/stacked-dev-brief-7"}"#,
        &ResumeInput {
            session_id: "stacked-dev-brief-7".to_owned(),
            feedback: "error: unused variable".to_owned(),
            workspace_path: "/repo/.yggdrasil-worktrees/stacked-dev-brief-7".to_owned(),
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
            r#"{{"workspace":{workspace_literal},"brief_id":"brief-7","reviewers":["sample-reviewer"],"dev_result":{dev_result_literal},"gate_result":{{"verdict":{{"outcome":"pass"}}}},"workflow_id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}}"#
        ),
        &ReviewRequest {
            workspace,
            brief_id: "brief-7".to_owned(),
            reviewers: vec!["sample-reviewer".to_owned()],
            dev_result,
            gate_result: GateResult {
                verdict: GateVerdict::Pass,
            },
            workflow_id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
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
            r#"{{"workspace":{workspace_literal},"repo_root":"/sample/repo","base_ref":"main","dev_result":{dev_result_literal}}}"#
        ),
        &LandInput {
            workspace,
            repo_root: "/sample/repo".to_owned(),
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

// --- BD-005 R8: the v2 pipeline payloads -------------------------------------

// Mirrors `codecs_core.scout_input_codec`.
#[test]
fn scout_input_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &format!(r#"{{"workspace":{workspace_literal},"prompt":"Scout this brief..."}}"#),
        &ScoutInput {
            workspace,
            prompt: "Scout this brief...".to_owned(),
        },
    )
}

// Mirrors `codecs_flow.review_input_codec`.
#[test]
fn review_input_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    assert_wire(
        &format!(r#"{{"workspace":{workspace_literal},"prompt":"Review this brief..."}}"#),
        &ReviewInput {
            workspace,
            prompt: "Review this brief...".to_owned(),
        },
    )
}

// Mirrors `aion_stacked_dev_io.scout_report_to_json`.
#[test]
fn scout_report_wire_shape() -> TestResult {
    let (literal, value) = scout_report();
    assert_wire(&literal, &value)
}

// Mirrors `aion_stacked_dev_io.dev_report_to_json`.
#[test]
fn dev_report_wire_shape() -> TestResult {
    let (literal, value) = dev_report();
    assert_wire(&literal, &value)
}

// Mirrors `aion_stacked_dev_io.review_report_to_json`.
#[test]
fn review_report_wire_shape() -> TestResult {
    let (literal, value) = review_report();
    assert_wire(&literal, &value)
}

// Mirrors `codecs_brief.brief_document_to_json` — a requirement carrying
// scout/dev/review blocks and a document carrying an execution block.
#[test]
fn brief_document_wire_shape() -> TestResult {
    let (literal, value) = brief_document();
    assert_wire(&literal, &value)
}

// Mirrors `codecs_brief.resolved_context_to_json`.
#[test]
fn resolved_context_wire_shape() -> TestResult {
    let (literal, value) = resolved_context();
    assert_wire(&literal, &value)
}

// Mirrors `codecs_flow.enrich_input_codec`, scout-stage variant.
#[test]
fn enrich_input_scout_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let (document_literal, document) = brief_document();
    let (report_literal, report) = scout_report();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"document":{document_literal},"enrichment":{{"stage":"scout","report":{report_literal}}}}}"#
        ),
        &EnrichInput {
            workspace,
            document,
            enrichment: Enrichment::Scout { report },
        },
    )
}

// Mirrors `codecs_flow.enrich_input_codec`, dev-stage variant.
#[test]
fn enrich_input_dev_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let (document_literal, document) = brief_document();
    let (report_literal, report) = dev_report();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"document":{document_literal},"enrichment":{{"stage":"dev","report":{report_literal}}}}}"#
        ),
        &EnrichInput {
            workspace,
            document,
            enrichment: Enrichment::Dev { report },
        },
    )
}

// Mirrors `codecs_flow.enrich_input_codec`, review-stage variant.
#[test]
fn enrich_input_review_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let (document_literal, document) = brief_document();
    let (report_literal, report) = review_report();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"document":{document_literal},"enrichment":{{"stage":"review","report":{report_literal}}}}}"#
        ),
        &EnrichInput {
            workspace,
            document,
            enrichment: Enrichment::Review { report },
        },
    )
}

// Mirrors `codecs_flow.enrich_input_codec`, execution variant — the block's
// gate and attestation are distinct objects (P1).
#[test]
fn enrich_input_execution_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let (document_literal, document) = brief_document();
    let (block_literal, block) = execution_block();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"document":{document_literal},"enrichment":{{"stage":"execution","block":{block_literal}}}}}"#
        ),
        &EnrichInput {
            workspace,
            document,
            enrichment: Enrichment::Execution { block },
        },
    )
}

// Mirrors `codecs_workflows.brief_dev_input_codec`.
#[test]
fn brief_dev_input_wire_shape() -> TestResult {
    let (workspace_literal, workspace) = workspace();
    let (document_literal, document) = brief_document();
    let (context_literal, context) = resolved_context();
    assert_wire(
        &format!(
            r#"{{"workspace":{workspace_literal},"document":{document_literal},"context":{context_literal},"verify_fix_cap":3,"round_backoff_ms":25}}"#
        ),
        &BriefDevInput {
            workspace,
            document,
            context,
            verify_fix_cap: 3,
            round_backoff_ms: 25,
        },
    )
}

// Mirrors `codecs_workflows.brief_dev_result_codec`.
#[test]
fn brief_dev_result_wire_shape() -> TestResult {
    let (scout_literal, scout) = scout_report();
    let (dev_literal, dev) = dev_report();
    let (review_literal, review) = review_report();
    assert_wire(
        &format!(
            r#"{{"scout":{scout_literal},"dev":{dev_literal},"review":{review_literal},"verify_rounds":2,"build_warm":{{"ok":true,"duration_ms":42}}}}"#
        ),
        &BriefDevResult {
            scout,
            dev,
            review,
            verify_rounds: 2,
            build_warm: BuildWarm {
                ok: true,
                duration_ms: 42,
            },
        },
    )
}

// Mirrors `codecs_workflows.stacked_dev_input_codec` — the reshaped input with
// the v2 brief document and resolved context.
#[test]
fn stacked_dev_input_wire_shape() -> TestResult {
    let (document_literal, brief_document) = brief_document();
    let (context_literal, resolved_context) = resolved_context();
    assert_wire(
        &format!(
            r#"{{"repo_root":"/abs/repo","brief_id":"brief-7","reviewers":["sample-reviewer"],"base_ref":"main","placement":"local","isolation":"worktree","brief_document":{document_literal},"resolved_context":{context_literal},"verify_fix_cap":3,"review_cap":3,"round_backoff_ms":25,"review_deadline_ms":60000}}"#
        ),
        &StackedDevInput {
            repo_root: "/abs/repo".to_owned(),
            brief_id: "brief-7".to_owned(),
            reviewers: vec!["sample-reviewer".to_owned()],
            base_ref: "main".to_owned(),
            placement: Placement::Local,
            isolation: Isolation::Worktree,
            brief_document,
            resolved_context,
            verify_fix_cap: 3,
            review_cap: 3,
            round_backoff_ms: 25,
            review_deadline_ms: 60000,
        },
    )
}

// Mirrors `codecs_workflows.stacked_dev_result_codec`.
#[test]
fn stacked_dev_result_wire_shape() -> TestResult {
    assert_wire(
        r#"{"branch":"stacked-dev-brief-7","merged_into":"main","session_id":"stacked-dev-brief-7","build_warm":{"ok":true,"duration_ms":42},"verify_rounds":2,"review_rounds":1}"#,
        &StackedDevResult {
            branch: "stacked-dev-brief-7".to_owned(),
            merged_into: "main".to_owned(),
            session_id: "stacked-dev-brief-7".to_owned(),
            build_warm: BuildWarm {
                ok: true,
                duration_ms: 42,
            },
            verify_rounds: 2,
            review_rounds: 1,
        },
    )
}

// --- BD-006: the dispatch + assemble_wave payloads ---------------------------

// Mirrors `codecs_dispatch.assemble_input_codec`.
#[test]
fn assemble_input_wire_shape() -> TestResult {
    assert_wire(
        r#"{"design_dir":"docs/design","wave":["BD-006","BD-005"]}"#,
        &AssembleInput {
            design_dir: "docs/design".to_owned(),
            wave: vec!["BD-006".to_owned(), "BD-005".to_owned()],
        },
    )
}

// Mirrors `codecs_dispatch` `wave_entry_to_json` — the brief document and the
// resolved context reuse BD-001's codecs.
#[test]
fn wave_entry_wire_shape() -> TestResult {
    let (document_literal, brief_document) = brief_document();
    let (context_literal, resolved_context) = resolved_context();
    assert_wire(
        &format!(r#"{{"brief_document":{document_literal},"resolved_context":{context_literal}}}"#),
        &WaveEntry {
            brief_document,
            resolved_context,
        },
    )
}

// Mirrors `codecs_dispatch.assembled_wave_codec`.
#[test]
fn assembled_wave_wire_shape() -> TestResult {
    let (document_literal, brief_document) = brief_document();
    let (context_literal, resolved_context) = resolved_context();
    assert_wire(
        &format!(
            r#"{{"entries":[{{"brief_document":{document_literal},"resolved_context":{context_literal}}}]}}"#
        ),
        &AssembledWave {
            entries: vec![WaveEntry {
                brief_document,
                resolved_context,
            }],
        },
    )
}

// Mirrors `codecs_dispatch.dispatch_input_codec` — all twelve required fields
// in declaration order, no defaults (ADR-001).
#[test]
fn dispatch_input_wire_shape() -> TestResult {
    assert_wire(
        r#"{"design_dir":"docs/design","wave":["BD-006"],"repo_root":"/abs/repo","base_ref":"main","reviewers":["sample-reviewer"],"placement":"local","isolation":"worktree","verify_fix_cap":3,"review_cap":3,"round_backoff_ms":25,"review_deadline_ms":60000,"halt_on_failure":true}"#,
        &DispatchInput {
            design_dir: "docs/design".to_owned(),
            wave: vec!["BD-006".to_owned()],
            repo_root: "/abs/repo".to_owned(),
            base_ref: "main".to_owned(),
            reviewers: vec!["sample-reviewer".to_owned()],
            placement: Placement::Local,
            isolation: Isolation::Worktree,
            verify_fix_cap: 3,
            review_cap: 3,
            round_backoff_ms: 25,
            review_deadline_ms: 60_000,
            halt_on_failure: true,
        },
    )
}

// Mirrors `codecs_dispatch.dispatch_result_codec` — one landed, one failed
// (embedding the stacked_dev error encoding under `error`), one skipped, byte
// for byte both directions.
#[test]
fn dispatch_result_wire_shape() -> TestResult {
    assert_wire(
        r#"{"outcomes":[{"outcome":"landed","brief_id":"BD-001","branch":"stacked-dev-BD-001","merged_into":"main"},{"outcome":"failed","brief_id":"BD-002","error":{"error":"stage_failed","stage":"gate","message":"boom"}},{"outcome":"skipped","brief_id":"BD-003","after":"BD-002"}]}"#,
        &DispatchResult {
            outcomes: vec![
                BriefOutcome::Landed {
                    brief_id: "BD-001".to_owned(),
                    branch: "stacked-dev-BD-001".to_owned(),
                    merged_into: "main".to_owned(),
                },
                BriefOutcome::Failed {
                    brief_id: "BD-002".to_owned(),
                    error: StackedDevError::StageFailed {
                        stage: "gate".to_owned(),
                        message: "boom".to_owned(),
                    },
                },
                BriefOutcome::Skipped {
                    brief_id: "BD-003".to_owned(),
                    after: "BD-002".to_owned(),
                },
            ],
        },
    )
}

// Mirrors `codecs_dispatch.dispatch_error_codec`, both variants.
#[test]
fn dispatch_error_wire_shape() -> TestResult {
    assert_wire(
        r#"{"error":"assembly_refused","message":"assemble_wave refused the wave: BD-006 depends on BD-005"}"#,
        &DispatchError::AssemblyRefused {
            message: "assemble_wave refused the wave: BD-006 depends on BD-005".to_owned(),
        },
    )?;
    assert_wire(
        r#"{"error":"dispatch_stage_failed","stage":"spawn_stacked_dev","message":"child engine failure"}"#,
        &DispatchError::DispatchStageFailed {
            stage: "spawn_stacked_dev".to_owned(),
            message: "child engine failure".to_owned(),
        },
    )
}
