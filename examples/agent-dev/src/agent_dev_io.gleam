//// Boundary types for the agent-dev pipeline — the authored source of truth
//// (ADR-014, types-first).
////
//// Declare types only here: `aion generate` derives the JSON codecs
//// (`src/agent_dev_codecs.gleam`) and the emitted `schemas/*.json` artifacts
//// from these types. Edit a type, run `aion generate`, and commit the type
//// with its regenerated artifacts together.
////
//// The scout/dev/review agent activities are deliberately absent from this
//// module: their wire contract is one prompt string in, one terminal-text
//// string out (the norn-harness contract), carried by a plain string codec
//// in `agent_dev/activities` — there is no record shape to declare.

/// The run's terminal disposition. Exhausting a loop budget is a disposition,
/// never a workflow error: the run completes and the workspace persists for
/// inspection.
pub type Disposition {
  Passed
  ReviewCapExhausted
  GateCapExhausted
}

/// The workflow's typed error: a stage that could not execute at all
/// (a failed activity dispatch, never a failed review or gate verdict —
/// those are recorded data).
pub type AgentDevError {
  AgentDevError(stage: String, message: String)
}

/// Answer of the `agent_dev_status` query: the live phase and round.
pub type AgentDevStatus {
  AgentDevStatus(phase: String, round: Int)
}

/// Result of one gate round: the measured verdict plus its diagnostics.
/// `pass: False` with empty diagnostics records a gate that never ran.
pub type GateDetail {
  GateDetail(pass: Bool, diagnostics: String)
}

/// The workflow's start input. Every field is required — the workflow bakes
/// no defaults; both loop caps are the caller's explicit budget.
pub type Input {
  Input(
    repo_url: String,
    base_ref: String,
    brief_id: String,
    brief: String,
    design_notes: String,
    acceptance: List(String),
    dev_review_cap: Int,
    gate_cap: Int,
  )
}

/// Input of the `land` activity: merge the workspace branch for a brief.
pub type LandInput {
  LandInput(workspace: Workspace, brief_id: String)
}

/// Result of the `land` activity.
pub type LandOutput {
  LandOutput(commit_sha: String)
}

/// The workflow's recorded result: the terminal disposition, the budgets
/// spent, the last review verdict and gate detail, and the workspace
/// coordinates (which persist on every disposition for inspection).
pub type Output {
  Output(
    disposition: Disposition,
    dev_review_rounds: Int,
    gate_rounds: Int,
    last_review: ReviewVerdict,
    gate_detail: GateDetail,
    branch: String,
    workspace_path: String,
  )
}

/// Input of the `provision` activity: check out `repo_url` at `base_ref`
/// into a fresh working branch for the brief.
pub type ProvisionInput {
  ProvisionInput(repo_url: String, base_ref: String, brief_id: String)
}

/// The review verdict decoded (defensively) from the review agent's terminal
/// text. `pass: True` only with zero blockers and production-ready work.
pub type ReviewVerdict {
  ReviewVerdict(pass: Bool, blockers: List(String), summary: String)
}

/// The provisioned workspace every downstream activity operates in.
pub type Workspace {
  Workspace(path: String, branch: String)
}
