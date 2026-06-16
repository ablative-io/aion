//! Wire types for the stacked-dev activity payloads.
//!
//! Every type here must serialize/deserialize **byte-compatibly** with the
//! Gleam codecs in `../src/stacked_dev/codecs_core.gleam` and
//! `../src/stacked_dev/codecs_flow.gleam` — those codecs are the authoritative
//! contract (field names, enum tag strings, and field order, since both sides
//! emit compact JSON in declaration order). `tests/wire_compat.rs` pins each
//! shape against literal JSON derived from the codec source; any drift must
//! fail there.

use serde::{Deserialize, Serialize};

/// Where the provisioned workspace runs.
///
/// Wire strings from `codecs_core.placement_to_string`: `"local"`/`"remote"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// The workspace runs on the local host.
    Local,
    /// The workspace runs on a remote host.
    Remote,
}

/// How the provisioned workspace is isolated from the source repository.
///
/// Wire strings from `codecs_core.isolation_to_string`:
/// `"worktree"`/`"copy"`/`"overlay"`/`"vm"`. Only `Worktree` has a working
/// implementation today; the rest are typed seams that fail loudly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// A git worktree of the source repository.
    Worktree,
    /// A full copy (typed seam, no implementation).
    Copy,
    /// An overlay filesystem (typed seam, no implementation).
    Overlay,
    /// An exchange VM (typed seam, no implementation).
    Vm,
}

impl Isolation {
    /// The wire name, used verbatim in failure messages exactly like
    /// `codecs_core.isolation_to_string`.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::Copy => "copy",
            Self::Overlay => "overlay",
            Self::Vm => "vm",
        }
    }
}

/// Input to the `provision_workspace` activity
/// (`codecs_core.provision_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProvisionInput {
    /// Absolute path of the repository to provision from.
    pub repo_root: String,
    /// Brief identifier; the branch is `stacked-dev-<brief_id>`.
    pub brief_id: String,
    /// Ref the provisioned branch is added under.
    pub base_ref: String,
    /// Where the workspace runs.
    pub placement: Placement,
    /// How the workspace is isolated.
    pub isolation: Isolation,
}

/// A provisioned, isolated workspace (`codecs_core.workspace_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    /// Absolute path of the workspace directory.
    pub path: String,
    /// Branch the workspace tracks.
    pub branch: String,
    /// Where the workspace runs.
    pub placement: Placement,
    /// How the workspace is isolated.
    pub isolation: Isolation,
}

/// Advisory warm-build outcome (`codecs_core.build_warm_to_json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildWarm {
    /// Whether `cargo build` exited zero. `false` forfeits the warm cache and
    /// never fails the run.
    pub ok: bool,
    /// Wall-clock duration of the build process.
    pub duration_ms: u64,
}

/// Input to the `dev` activity (`codecs_core.dev_input_to_json`). The four
/// document strings are gone; the projected dev prompt is built in workflow
/// code from the BD-002 projections and rides as one field (BD-003).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DevInput {
    /// The workspace the dev agent works in.
    pub workspace: Workspace,
    /// The projected dev prompt.
    pub prompt: String,
}

/// Input to the `scout` activity (`codecs_core.scout_input_codec`). The
/// projected scout prompt rides as one field; the scout runs read-only in the
/// `<branch>-scout` session (BD-003).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoutInput {
    /// The workspace the scout explores (read-only).
    pub workspace: Workspace,
    /// The projected scout prompt.
    pub prompt: String,
}

/// Input to the `dev_review` activity (`codecs_flow.review_input_codec`). The
/// projected review prompt rides as one field; the reviewer runs in the
/// `<branch>-review` session, NEVER the dev session (BD-003, CN4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewInput {
    /// The workspace under review.
    pub workspace: Workspace,
    /// The projected review prompt.
    pub prompt: String,
}

/// Result of a dev round (`codecs_core.dev_result_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DevResult {
    /// Agent session id; later rounds resume this session.
    pub session_id: String,
    /// Files the round touched.
    pub files_touched: Vec<String>,
    /// Human-readable summary of the round.
    pub summary: String,
}

/// Tagged input envelope for the concurrent startup fan-out
/// (`codecs_core.startup_task_codec`). `workflow.all` collects a homogeneous
/// activity list, so `warm_build` and `dev` share this type; each activity
/// receives only its own variant.
///
/// Wire shapes: `{"task":"warm_build","workspace":{..}}` and
/// `{"task":"dev","dev_input":{..}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "task")]
pub enum StartupTask {
    /// Dispatched to the `warm_build` activity.
    #[serde(rename = "warm_build")]
    WarmBuild {
        /// Workspace whose build cache is warmed.
        workspace: Workspace,
    },
    /// Dispatched to the `dev` activity.
    #[serde(rename = "dev")]
    Dev {
        /// The dev round input.
        dev_input: DevInput,
    },
}

/// Tagged output envelope mirroring [`StartupTask`]
/// (`codecs_core.startup_result_codec`): `warm_build` answers the
/// `warm_build` variant, `dev` answers the `dev` variant carrying the dev
/// report (BD-003).
///
/// Wire shapes: `{"task":"warm_build","build_warm":{..}}` and
/// `{"task":"dev","dev_report":{..}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "task")]
pub enum StartupResult {
    /// `warm_build`'s advisory outcome.
    #[serde(rename = "warm_build")]
    Warmed {
        /// The advisory warm-build outcome.
        build_warm: BuildWarm,
    },
    /// `dev`'s structured dev report.
    #[serde(rename = "dev")]
    Developed {
        /// The dev round's report.
        dev_report: DevReport,
    },
}

/// Input to the `scoped_checks` activity (`codecs_core.scoped_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopedInput {
    /// The workspace to check.
    pub workspace: Workspace,
    /// Files the dev round touched, seeding the affected-set query.
    pub files_touched: Vec<String>,
}

/// Verdict of one scoped check round (`codecs_core` `check_verdict_to_json`).
///
/// Wire shapes: `{"outcome":"pass"}` and
/// `{"outcome":"fail","diagnostics":".."}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum CheckVerdict {
    /// The scoped checks passed.
    Pass,
    /// The scoped checks failed with diagnostics.
    Fail {
        /// Combined diagnostics output of the failing check run.
        diagnostics: String,
    },
}

/// Result of the `scoped_checks` activity
/// (`codecs_core.check_result_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    /// The verdict of the check run.
    pub verdict: CheckVerdict,
    /// Affected packages the dependency graph reported (empty on the
    /// workspace-wide fallback).
    pub affected_modules: Vec<String>,
    /// The scope that actually ran — a loud workspace-wide fallback is
    /// visible data, never a silent widening.
    pub checked_scope: String,
}

/// Input to the `dev_resume` activity (`codecs_core.resume_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResumeInput {
    /// Session to resume.
    pub session_id: String,
    /// Scoped-check diagnostics or encoded review notes.
    pub feedback: String,
    /// Worktree path so resume launches in the same directory as the dev round.
    pub workspace_path: String,
}

/// Scope of the authoritative gate run (`codecs_flow` `gate_scope_to_json`).
///
/// Wire shapes: `{"kind":"workspace_wide"}` and
/// `{"kind":"affected_closure","modules":[..]}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GateScope {
    /// The full workspace sweep — the only implemented scope.
    WorkspaceWide,
    /// Typed seam for a graph-derived closure; terminal until implemented.
    AffectedClosure {
        /// Modules of the affected closure.
        modules: Vec<String>,
    },
}

/// Input to the `full_checks` activity (`codecs_flow.gate_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateInput {
    /// The workspace to gate.
    pub workspace: Workspace,
    /// Files the dev rounds touched.
    pub files_touched: Vec<String>,
    /// The gate scope.
    pub scope: GateScope,
}

/// Verdict of the authoritative gate (`codecs_flow` `gate_verdict_to_json`).
///
/// Wire shapes: `{"outcome":"pass"}` and `{"outcome":"fail","report":".."}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum GateVerdict {
    /// The gate passed.
    Pass,
    /// The gate executed and failed; the report is recorded data.
    Fail {
        /// Combined output of the failing workspace sweep.
        report: String,
    },
}

/// Output of the `full_checks` activity (`codecs_flow.gate_result_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    /// The gate verdict.
    pub verdict: GateVerdict,
}

/// Input to the `request_review` activity
/// (`codecs_flow.review_request_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewRequest {
    /// The workspace under review.
    pub workspace: Workspace,
    /// The brief being reviewed.
    pub brief_id: String,
    /// Member names or UUIDs to request review from.
    pub reviewers: Vec<String>,
    /// The dev result whose work is reviewed.
    pub dev_result: DevResult,
    /// The gate result accompanying the request.
    pub gate_result: GateResult,
    /// The parent workflow's unique identifier, used to construct the signal
    /// command reviewers send to approve or request changes.
    pub workflow_id: String,
}

/// Output of the `request_review` activity (`codecs_flow.review_ack_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewAck {
    /// Identifier of the emitted review request.
    pub request_id: String,
}

/// Input to the `land` activity (`codecs_flow.land_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LandInput {
    /// The approved workspace.
    pub workspace: Workspace,
    /// The main repository the merge runs from (`yg branch merge` removes
    /// the branch's worktree as part of landing, so it must not run from
    /// inside it).
    pub repo_root: String,
    /// The tree parent the branch merges into.
    pub base_ref: String,
    /// The dev result being landed.
    pub dev_result: DevResult,
}

/// Output of the `land` activity (`codecs_flow.landed_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Landed {
    /// The branch that was merged.
    pub branch: String,
    /// The tree parent it merged into.
    pub merged_into: String,
}

/// Input to the `teardown_workspace` activity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TeardownInput {
    /// The workspace to tear down.
    pub workspace: Workspace,
    /// The main repository root (used for local worktree cleanup).
    pub repo_root: String,
}

/// Output of the `teardown_workspace` activity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TornDown {
    /// The branch that was cleaned up.
    pub branch: String,
    /// Whether the workspace directory was removed.
    pub cleaned: bool,
}

// --- stage-contract reports --------------------------------------------------
//
// The three generated stage-contract shapes (`aion_stacked_dev_io`
// scout_report/dev_report/review_report). The scout and review reports are the
// outputs the `scout` and `dev_review` handlers parse from norn; the dev report
// is what the `dev`/`dev_resume` handlers parse.

/// Scout-stage report (`aion_stacked_dev_io.scout_report_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoutReport {
    /// 2-3 sentences orienting the implementer.
    pub summary: String,
    /// One entry per requirement.
    pub enrichments: Vec<ScoutEnrichment>,
    /// Concrete checks discovered during exploration.
    pub verification: Vec<String>,
}

/// One requirement's scout findings
/// (`aion_stacked_dev_io.scout_report_enrichments_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoutEnrichment {
    /// R# id.
    pub id: String,
    /// Key files for this requirement.
    #[serde(default)]
    pub files: Vec<String>,
    /// Conventions, signatures, gotchas.
    #[serde(default)]
    pub context: Vec<String>,
    /// How to implement this requirement.
    #[serde(default)]
    pub approach: String,
    /// Non-obvious notes; empty if none.
    #[serde(default)]
    pub notes: String,
}

/// Dev-stage report (`aion_stacked_dev_io.dev_report_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DevReport {
    /// 1-2 sentences on what was done.
    pub summary: String,
    /// Conventional-commits message for this round's commit.
    pub commit_message: String,
    /// One entry per requirement.
    pub enrichments: Vec<DevEnrichment>,
    /// What the agent believes — never the gate (P1).
    pub attestation: Attestation,
}

/// One requirement's dev record
/// (`aion_stacked_dev_io.dev_report_enrichments_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DevEnrichment {
    /// R# id.
    pub id: String,
    /// Implemented or blocked.
    pub status: DevStatus,
    /// The files this requirement touched.
    pub files_changed: Vec<FileChange>,
    /// How the requirement was met.
    pub how: String,
    /// Empty if the scouted plan was followed; otherwise what changed and why.
    pub deviation: String,
    /// Delivery claim per C# assigned to this requirement.
    pub checklist: Vec<ChecklistClaim>,
    /// Satisfaction claim per S# assigned to this requirement.
    pub stories: Vec<StoryClaim>,
}

/// Outcome of the dev stage for one requirement (wire strings
/// `implemented`/`blocked`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DevStatus {
    /// The requirement was implemented.
    Implemented,
    /// The requirement could not be implemented.
    Blocked,
}

/// One file the dev stage touched
/// (`aion_stacked_dev_io.dev_report_enrichments_item_files_changed_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileChange {
    /// The file path.
    pub path: String,
    /// What was done to the file.
    pub change: ChangeKind,
    /// A short note.
    pub note: String,
}

/// What the dev stage did to a file (wire strings
/// `created`/`modified`/`deleted`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// The file was created.
    Created,
    /// The file was modified.
    Modified,
    /// The file was deleted.
    Deleted,
}

/// The dev agent's delivery claim for one checklist item
/// (`aion_stacked_dev_io.dev_report_enrichments_item_checklist_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChecklistClaim {
    /// C# id.
    pub id: String,
    /// Whether the item is delivered.
    pub done: bool,
    /// A short note.
    pub note: String,
}

/// The dev agent's satisfaction claim for one story
/// (`aion_stacked_dev_io.dev_report_enrichments_item_stories_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoryClaim {
    /// S# id.
    pub id: String,
    /// Whether the story is satisfied.
    pub satisfied: bool,
    /// A short note.
    pub note: String,
}

/// A single attestation claim: the agent's asserted yes/no answer about its
/// own diff. Deliberately distinct from a `bool` gate measurement — a claim is
/// believed, the gate is measured, and the design treats their divergence as a
/// review signal. Transparent on the wire (serializes as a bare boolean).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Claim(pub bool);

/// The dev attestation — what the agent believes, never trusted as the gate
/// (`aion_stacked_dev_io.dev_report_attestation_to_json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    /// No unwrap/expect/panic/todo in library code.
    pub no_panics: Claim,
    /// No unsafe blocks added.
    pub no_unsafe: Claim,
    /// All SHALL NOT boundaries observed.
    pub boundaries_respected: Claim,
    /// The agent's belief — the workflow measures the truth at the gate.
    pub tests_pass: Claim,
}

/// Review-stage report (`aion_stacked_dev_io.review_report_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewReport {
    /// The honest overall read.
    pub summary: String,
    /// Conventional-commits message for the harden commit; empty if nothing
    /// changed.
    pub commit_message: String,
    /// One entry per requirement.
    pub enrichments: Vec<ReviewEnrichment>,
    /// The brief's cross-cutting verification steps, each executed.
    pub verification: Vec<ReviewVerification>,
}

/// One requirement's review verdict
/// (`aion_stacked_dev_io.review_report_enrichments_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewEnrichment {
    /// R# id.
    pub id: String,
    /// How the implementation relates to the spec after review.
    pub alignment: Alignment,
    /// One verdict per acceptance criterion.
    pub acceptance: Vec<AcceptanceVerdict>,
    /// C-numbers verified delivered.
    pub checklist: Vec<String>,
    /// S-numbers verified satisfied.
    pub stories: Vec<String>,
    /// Everything found, fixed or not.
    pub issues: Vec<String>,
    /// What the harden pass changed.
    pub fixes: Vec<String>,
}

/// How the implementation relates to the spec after review (wire strings
/// `aligned`/`drifted`/`fixed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Alignment {
    /// Implementation matches spec.
    Aligned,
    /// Implementation does not match spec and remains so (a failing state).
    Drifted,
    /// It drifted and the harden pass corrected it.
    Fixed,
}

/// The reviewer's verdict on one acceptance criterion
/// (`aion_stacked_dev_io.review_report_enrichments_item_acceptance_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceVerdict {
    /// The acceptance criterion verbatim.
    pub criterion: String,
    /// Whether it is met.
    pub met: bool,
    /// What in the diff proves it — a source location or test name.
    pub evidence: String,
}

/// One brief-level verification step's outcome
/// (`aion_stacked_dev_io.review_report_verification_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewVerification {
    /// The verification step verbatim.
    pub criterion: String,
    /// Whether it passed.
    pub passed: bool,
    /// A short note.
    pub note: String,
}

// --- brief document, resolved context, and enrich payload --------------------
//
// Hand-written mirrors of `codecs_brief`/`codecs_brief_blocks`. Optional
// enrichment fields are omitted from the wire when absent (the Gleam codec
// omits a `None` key entirely), so `skip_serializing_if` keeps the byte shape
// matching.

/// A v2 brief document (`codecs_brief.brief_document_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BriefDocument {
    /// Brief id.
    pub id: String,
    /// Cluster name.
    pub cluster: String,
    /// Short imperative title.
    pub title: String,
    /// Brief ids that must land first.
    pub depends_on: Vec<String>,
    /// External blockers that are not briefs.
    pub blocked_by: Vec<String>,
    /// Checklist items this brief covers.
    pub checklist: Vec<String>,
    /// User stories this brief addresses.
    pub stories: Vec<String>,
    /// ADR ids that bind this brief.
    pub design_anchor: Vec<String>,
    /// What this brief delivers and why.
    pub purpose: String,
    /// Plain-language description of the work.
    pub task: String,
    /// The numbered requirements.
    pub requirements: Vec<BriefRequirement>,
    /// SHALL NOT statements.
    pub boundaries: Vec<String>,
    /// Cross-cutting verification steps.
    pub verification: Vec<String>,
    /// The run-level execution record, appended by the pipeline before land.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionBlock>,
}

/// One numbered requirement of a brief
/// (`codecs_brief.brief_requirement_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BriefRequirement {
    /// R# id.
    pub id: String,
    /// Short imperative title.
    pub title: String,
    /// The EARS-notation spec.
    pub spec: String,
    /// Observable acceptance conditions.
    pub acceptance: Vec<String>,
    /// The authored file plan.
    pub files: RequirementFiles,
    /// C-numbers this requirement delivers.
    pub checklist: Vec<String>,
    /// S-numbers this requirement addresses.
    pub stories: Vec<String>,
    /// Scout-stage findings, appended by the pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scout: Option<ScoutBlock>,
    /// Dev-stage record, appended by the pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev: Option<DevBlock>,
    /// Review-stage verdict, appended by the pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewBlock>,
}

/// The authored file plan of a requirement
/// (`codecs_brief.requirement_files_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequirementFiles {
    /// Files to create.
    pub create: Vec<String>,
    /// Files to modify.
    pub modify: Vec<String>,
    /// Files to delete.
    pub delete: Vec<String>,
}

/// Scout-stage findings appended to a requirement
/// (`codecs_brief_blocks.scout_block_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoutBlock {
    /// Key files.
    pub files: Vec<String>,
    /// Conventions, signatures, gotchas.
    pub context: Vec<String>,
    /// How to implement.
    pub approach: String,
    /// Non-obvious notes.
    pub notes: String,
}

/// Dev-stage record appended to a requirement
/// (`codecs_brief_blocks.dev_block_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DevBlock {
    /// Implemented or blocked.
    pub status: DevStatus,
    /// The files this requirement touched.
    pub files_changed: Vec<FileChange>,
    /// How the requirement was met.
    pub how: String,
    /// Declared deviation, if any.
    pub deviation: String,
    /// Per-C# delivery claims.
    pub checklist: Vec<ChecklistClaim>,
    /// Per-S# satisfaction claims.
    pub stories: Vec<StoryClaim>,
}

/// Review-stage verdict appended to a requirement
/// (`codecs_brief_blocks.review_block_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewBlock {
    /// How the implementation relates to the spec.
    pub alignment: Alignment,
    /// Per-criterion verdicts.
    pub acceptance: Vec<AcceptanceVerdict>,
    /// C-numbers verified.
    pub checklist: Vec<String>,
    /// S-numbers verified.
    pub stories: Vec<String>,
    /// Everything found.
    pub issues: Vec<String>,
    /// What the harden pass changed.
    pub fixes: Vec<String>,
}

/// The run-level execution record (`codecs_brief_blocks.execution_block_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionBlock {
    /// Run status.
    pub status: ExecutionStatus,
    /// The aion workflow id.
    pub workflow_id: String,
    /// The stacked branch the work rode.
    pub branch: String,
    /// Deterministic agent session id.
    pub session_id: String,
    /// What the workflow MEASURED at the gate.
    pub gate: GateBlock,
    /// What the dev agent BELIEVED — never the gate (P1).
    pub attestation: Attestation,
    /// The human (or quorum) verdict.
    pub review_verdict: ExecutionVerdict,
    /// Commit hash on the target branch; empty until landed (ADR-009).
    pub landed_commit: String,
    /// The branch landed into; empty until landed.
    pub merged_into: String,
    /// ISO 8601; empty while in flight.
    pub completed_at: String,
}

/// Run status recorded in the execution block (wire strings
/// `in_flight`/`landed`/`failed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// The run is in flight.
    InFlight,
    /// The run landed.
    Landed,
    /// The run failed.
    Failed,
}

/// Human (or quorum) review verdict recorded in the execution block (wire
/// strings `approved`/`changes_requested`/`rejected`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionVerdict {
    /// Approved.
    Approved,
    /// Changes requested.
    ChangesRequested,
    /// Rejected.
    Rejected,
}

/// What the workflow measured at the authoritative gate
/// (`codecs_brief_blocks.gate_block_to_json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateBlock {
    /// `cargo fmt` clean.
    pub fmt: bool,
    /// `cargo clippy` clean.
    pub clippy: bool,
    /// `cargo test` clean.
    pub tests: bool,
    /// How many gate-failure fix rounds it took.
    pub fix_rounds: i64,
}

/// The pre-resolved reference context
/// (`codecs_brief.resolved_context_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedContext {
    /// The brief's anchored ADR texts.
    pub adrs: Vec<ResolvedAdr>,
    /// The brief's C# texts.
    pub checklist: Vec<ResolvedItem>,
    /// The brief's S# texts.
    pub stories: Vec<ResolvedItem>,
    /// The cluster constraints.
    pub constraints: Vec<ResolvedItem>,
    /// The cluster intention.
    pub intention: String,
    /// The design file reference.
    pub design_path: String,
    /// Where the briefed work came from.
    pub provenance: ResolvedProvenance,
}

/// One ADR resolved to its text (`codecs_brief` `resolved_adr_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedAdr {
    /// ADR id.
    pub id: String,
    /// ADR title.
    pub title: String,
    /// The decision text.
    pub decision: String,
    /// The verbatim quote, if any.
    pub quote: String,
    /// Who decided it.
    pub decided_by: String,
}

/// One checklist item, story, or constraint resolved to its text
/// (`codecs_brief` `resolved_item_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedItem {
    /// The item id.
    pub id: String,
    /// The resolved text.
    pub text: String,
}

/// The roadmap requester and their verbatim words
/// (`codecs_brief` `resolved_provenance_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedProvenance {
    /// Who requested the work.
    pub requested_by: String,
    /// Their verbatim words.
    pub quote: String,
}

/// Input to the `enrich_brief` activity (`codecs_flow.enrich_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnrichInput {
    /// The workspace whose worktree holds the brief file.
    pub workspace: Workspace,
    /// The brief document the workflow currently carries.
    pub document: BriefDocument,
    /// The stage payload to append.
    pub enrichment: Enrichment,
}

/// The stage payload appended by one `enrich_brief` call
/// (`codecs_flow` `enrichment_to_json`). The stage report rides under
/// `"report"` for the three stage variants and the execution block under
/// `"block"` for the execution variant.
///
/// Wire shapes: `{"stage":"scout","report":{..}}`, `{"stage":"dev",..}`,
/// `{"stage":"review",..}`, `{"stage":"execution","block":{..}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum Enrichment {
    /// A scout report.
    Scout {
        /// The scout report.
        report: ScoutReport,
    },
    /// A dev report.
    Dev {
        /// The dev report.
        report: DevReport,
    },
    /// A review report.
    Review {
        /// The review report.
        report: ReviewReport,
    },
    /// The execution block.
    Execution {
        /// The execution block.
        block: ExecutionBlock,
    },
}

/// Input to the `brief_dev` child workflow
/// (`codecs_workflows.brief_dev_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BriefDevInput {
    /// The provisioned workspace.
    pub workspace: Workspace,
    /// The v2 brief document.
    pub document: BriefDocument,
    /// The pre-resolved reference context.
    pub context: ResolvedContext,
    /// The verify-fix loop cap.
    pub verify_fix_cap: i64,
    /// The durable backoff between fix rounds.
    pub round_backoff_ms: i64,
}

/// Output of the `brief_dev` child workflow
/// (`codecs_workflows.brief_dev_result_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BriefDevResult {
    /// The scout report.
    pub scout: ScoutReport,
    /// The converged dev report.
    pub dev: DevReport,
    /// The review report.
    pub review: ReviewReport,
    /// How many verify rounds it took.
    pub verify_rounds: i64,
    /// The advisory warm-build outcome.
    pub build_warm: BuildWarm,
}

/// Input to the `stacked_dev` top-level workflow
/// (`codecs_workflows.stacked_dev_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StackedDevInput {
    /// The repository the worktree is provisioned from.
    pub repo_root: String,
    /// Brief identifier; the branch is `stacked-dev-<brief_id>`.
    pub brief_id: String,
    /// Member names or UUIDs to request review from.
    pub reviewers: Vec<String>,
    /// Ref the provisioned branch is added under.
    pub base_ref: String,
    /// Where the workspace runs.
    pub placement: Placement,
    /// How the workspace is isolated.
    pub isolation: Isolation,
    /// The v2 brief document.
    pub brief_document: BriefDocument,
    /// The pre-resolved reference context.
    pub resolved_context: ResolvedContext,
    /// The verify-fix loop cap.
    pub verify_fix_cap: i64,
    /// The review loop cap.
    pub review_cap: i64,
    /// The durable backoff between rounds.
    pub round_backoff_ms: i64,
    /// The durable review deadline.
    pub review_deadline_ms: i64,
}

/// Output of a landed `stacked_dev` run
/// (`codecs_workflows.stacked_dev_result_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StackedDevResult {
    /// The branch that was merged.
    pub branch: String,
    /// The tree parent it merged into.
    pub merged_into: String,
    /// The deterministic agent session id.
    pub session_id: String,
    /// The advisory warm-build outcome.
    pub build_warm: BuildWarm,
    /// How many verify rounds it took.
    pub verify_rounds: i64,
    /// How many review rounds it took.
    pub review_rounds: i64,
}

// --- assemble_wave payloads (BD-006) -----------------------------------------

/// Input to the `assemble_wave` activity
/// (`codecs_dispatch.assemble_input_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssembleInput {
    /// Directory holding the ledgers and cluster documents to resolve against.
    pub design_dir: String,
    /// The wave as an ordered list of brief ids.
    pub wave: Vec<String>,
}

/// One assembled wave entry (`codecs_dispatch` `wave_entry_to_json`): a
/// resolved v2 brief document and the reference context resolved for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveEntry {
    /// The decoded brief document.
    pub brief_document: BriefDocument,
    /// The pre-resolved reference context.
    pub resolved_context: ResolvedContext,
}

/// Output of the `assemble_wave` activity
/// (`codecs_dispatch.assembled_wave_codec`): the wave entries in dependency
/// order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssembledWave {
    /// The ordered wave entries.
    pub entries: Vec<WaveEntry>,
}
