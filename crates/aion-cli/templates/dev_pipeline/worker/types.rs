//! Wire types for the dev-pipeline activity payloads.
//!
//! Every type here must serialize/deserialize **byte-compatibly** with the
//! Gleam codecs in `../src/{{name}}/codecs_core.gleam` and
//! `../src/{{name}}/codecs_flow.gleam` — those codecs are the authoritative
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
    /// Brief identifier; the branch is `<project>-<brief_id>`.
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

/// Input to the `dev` activity (`codecs_core.dev_input_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DevInput {
    /// The workspace the dev agent works in.
    pub workspace: Workspace,
    /// The brief text.
    pub brief: String,
    /// Design context.
    pub design: String,
    /// Checklist context.
    pub checklist: String,
    /// Story references.
    pub stories: Vec<String>,
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
/// `warm_build` variant, `dev` answers the `dev` variant.
///
/// Wire shapes: `{"task":"warm_build","build_warm":{..}}` and
/// `{"task":"dev","dev_result":{..}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "task")]
pub enum StartupResult {
    /// `warm_build`'s advisory outcome.
    #[serde(rename = "warm_build")]
    Warmed {
        /// The advisory warm-build outcome.
        build_warm: BuildWarm,
    },
    /// `dev`'s structured result.
    #[serde(rename = "dev")]
    Developed {
        /// The dev round result.
        dev_result: DevResult,
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
    /// The dev result whose work is reviewed.
    pub dev_result: DevResult,
    /// The gate result accompanying the request.
    pub gate_result: GateResult,
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
    /// The dev result being landed.
    pub dev_result: DevResult,
}

/// Output of the `land` activity (`codecs_flow.landed_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Landed {
    /// URL of the submitted PR.
    pub pr_url: String,
    /// The merge commit of the landed stack.
    pub merge_commit: String,
}
