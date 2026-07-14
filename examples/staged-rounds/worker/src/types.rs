//! Wire types for the staged-rounds SHELL activity payloads.
//!
//! Every type here serializes/deserializes byte-compatibly with the AWL
//! record declarations in `../awl/staged_rounds.awl` — the document is the
//! authoritative contract (field names in `snake_case`, `String?` optionals
//! OMITTED from JSON when absent, lists tolerated missing on decode). The
//! four DRIVEN AGENT activities need no wire types here beyond what the
//! commit/harness seams read: their input is the structured context JSON the
//! workflow encoded (assembled into the prompt by
//! [`crate::harness::ProfiledNornHarness`]) and their output is produced by
//! Norn against the embedded `--output-schema`.

use serde::{Deserialize, Serialize};

/// Review-finding severity (`Finding.severity` on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Evidence that mechanically rejects the item's round.
    Blocking,
    /// Recorded evidence that does not itself reject the round.
    Advisory,
}

/// A verdict's asserted overall (`ItemVerdict.overall` on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    /// The reviewer asserts acceptance.
    Accept,
    /// The reviewer asserts rejection.
    Reject,
}

/// One configured gate command: run in an item workspace, pass = exit 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCommand {
    /// The operator's name for the command (rides in diagnostics).
    pub name: String,
    /// The argv to execute (`argv[0]` is the executable).
    pub argv: Vec<String>,
}

/// One planned work item: scope fences, a phase tag, and dependency ids.
/// `feedback` starts empty; `fold_phase` writes rejected items' reviewer
/// feedback into it for the next round's dev turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    /// Git-ref-safe slug naming the item (and its branch).
    pub id: String,
    /// One-line item title.
    pub title: String,
    /// The one-sentence goal the dev agent implements.
    pub goal: String,
    /// Files/dirs the item MAY touch.
    #[serde(default)]
    pub scope_in: Vec<String>,
    /// Hard walls the item must never touch.
    #[serde(default)]
    pub scope_out: Vec<String>,
    /// Phase tag: 1 = no prerequisites.
    pub phase: i64,
    /// Ids of items whose merged output this item needs.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Reviewer feedback attached by `fold_phase` when the item cycles.
    #[serde(default)]
    pub feedback: String,
}

/// The planner's typed plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The plan's one-paragraph summary.
    pub summary: String,
    /// The phased, scope-fenced work items.
    #[serde(default)]
    pub items: Vec<WorkItem>,
}

/// One accepted, committed item awaiting merge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoneItem {
    /// The accepted item's id.
    pub item_id: String,
    /// The branch carrying the item's committed work.
    pub branch: String,
    /// The commit the item's worktree started from.
    pub base_commit: String,
    /// The dev report's summary for the item.
    pub summary: String,
}

/// The one loop-carried value: released/blocked/done partitions plus the
/// run's accumulated adverse evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseState {
    /// Items released for the next round's fan-out.
    #[serde(default)]
    pub ready: Vec<WorkItem>,
    /// Items whose dependencies are not yet done.
    #[serde(default)]
    pub blocked: Vec<WorkItem>,
    /// Accepted items awaiting merge.
    #[serde(default)]
    pub done: Vec<DoneItem>,
    /// Accumulated adverse evidence across rounds.
    #[serde(default)]
    pub evidence: String,
}

/// A provisioned per-item worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionedItem {
    /// The item this worktree serves.
    pub item: WorkItem,
    /// The worktree's absolute path.
    pub workspace_path: String,
    /// The item branch checked out there.
    pub branch: String,
    /// The commit the worktree is based on (merge base with the base
    /// branch when reused across rounds).
    pub base_commit: String,
}

/// One acceptance-relevant claim in an [`ItemReport`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemClaim {
    /// The acceptance-relevant point being claimed.
    pub criterion: String,
    /// How the dev agent says it satisfied the point.
    pub how: String,
}

/// The dev agent's structured report for one item round.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemReport {
    /// The item the report covers.
    pub item_id: String,
    /// The round's summary.
    pub summary: String,
    /// The branch head embodying the work — REWRITTEN by the machinery
    /// commit after the turn; the agent never runs git.
    #[serde(default)]
    pub commits: Vec<String>,
    /// Per-criterion claims.
    #[serde(default)]
    pub claims: Vec<ItemClaim>,
}

/// One item's dev fan-out result: the provisioned coordinates plus the
/// report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevItemResult {
    /// The item the result covers.
    pub item: WorkItem,
    /// The item worktree the work happened in.
    pub workspace_path: String,
    /// The item branch.
    pub branch: String,
    /// The base commit the reviewer diffs against.
    pub base_commit: String,
    /// The dev agent's report.
    pub report: ItemReport,
}

/// One concrete finding in an [`ItemVerdict`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Whether this finding is blocking or advisory.
    pub severity: Severity,
    /// The concise finding title used in formatted adverse evidence.
    pub title: String,
    /// The detailed file/line or command evidence supplied by the reviewer.
    pub evidence: String,
}

/// One item's review verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemVerdict {
    /// The item the verdict covers.
    pub item_id: String,
    /// The reviewer's asserted overall.
    pub overall: Overall,
    /// The reviewer's concrete findings.
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// Its rejection reason. Both an omitted AWL optional and an explicit
    /// `null` deserialize to `None`; `None` re-serializes as an OMITTED key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<String>,
}

/// One conflicted merge, captured with the merge left IN PROGRESS in the
/// integration worktree for the remediator to resolve in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeConflict {
    /// The item whose branch conflicted.
    pub item_id: String,
    /// The conflicting branch.
    pub branch: String,
    /// The conflicted files (`git diff --name-only --diff-filter=U`).
    #[serde(default)]
    pub files: Vec<String>,
    /// Clipped merge output for the remediator's context.
    pub detail: String,
}

/// The merge loop's one threaded value, returned whole by `merge_branches`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeState {
    /// The run's integration branch.
    pub integration_branch: String,
    /// The integration worktree (the remediator's workspace).
    pub workspace_path: String,
    /// Branches fully merged so far.
    #[serde(default)]
    pub merged: Vec<String>,
    /// The outstanding conflicts (empty = merge complete).
    #[serde(default)]
    pub conflicts: Vec<MergeConflict>,
    /// Accumulated merge evidence.
    #[serde(default)]
    pub evidence: String,
}

/// Named-argument input to `provision_item`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionItemInput {
    /// The run's root directory (`<repo_root>/.staged-rounds/<workflow id>`).
    pub run_root: String,
    /// The repository the worktree is created from.
    pub repo_root: String,
    /// The branch item branches are created on top of.
    pub base_branch: String,
    /// The item to provision.
    pub item: WorkItem,
    /// The run's accepted items so far. The item's `depends_on` entries are
    /// resolved against this list and their branches merged into a fresh
    /// worktree — `depends_on` promises the planner MERGED output, and this
    /// is where that promise is delivered.
    #[serde(default)]
    pub done: Vec<DoneItem>,
}

/// Named-argument input to `fold_phase`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldPhaseInput {
    /// The state carried from the previous round (or the empty seed).
    pub prior: PhaseState,
    /// Newly planned items to partition into ready/blocked (the plan seed).
    #[serde(default)]
    pub incoming: Vec<WorkItem>,
    /// This round's dev fan-out results.
    #[serde(default)]
    pub dev: Vec<DevItemResult>,
    /// This round's verdicts, matched to `dev` by `item_id`.
    #[serde(default)]
    pub verdicts: Vec<ItemVerdict>,
}

/// Named-argument input to `merge_branches`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeBranchesInput {
    /// The run's root directory (the integration worktree lives under it).
    pub run_root: String,
    /// The repository being merged into.
    pub repo_root: String,
    /// The branch the integration branch is created from.
    pub base_branch: String,
    /// The accepted items whose branches merge, in order.
    #[serde(default)]
    pub done: Vec<DoneItem>,
    /// Evidence carried from the phase loop or the previous merge pass.
    #[serde(default)]
    pub prior_evidence: String,
    /// The latest remediation summary ("" on the first pass).
    #[serde(default)]
    pub remediation: String,
}
