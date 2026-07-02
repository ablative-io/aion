//// Boundary types for the {{name}} agent loop — the authored source of
//// truth (ADR-014, types-first).
////
//// Declare types only here: `aion generate` derives the JSON codecs
//// (`src/{{name}}_codecs.gleam`) and the emitted `schemas/*.json` artifacts
//// from these types. Edit a type, run `aion generate`, and commit the type
//// with its regenerated artifacts together. The worker's `StepInput` and
//// `StepOutput` structs (`worker/src/main.rs`) mirror the step types below —
//// keep them in step when you change one.

/// The workflow's start input: a prompt per agent step plus the human-review
/// deadline. Every deadline is caller-chosen — the workflow bakes no
/// defaults (the per-step activities run unbounded until the worker answers).
pub type Input {
  Input(
    task_id: String,
    scout_prompt: String,
    act_prompt: String,
    verify_prompt: String,
    review_timeout_ms: Int,
  )
}

/// Input handed to each parameterised agent step. `prompt` is the step's
/// instruction; `context` carries the prior step's output so the worker-side
/// agent can build on it. The workflow treats both as opaque text.
pub type StepInput {
  StepInput(task_id: String, prompt: String, context: String)
}

/// Output of an agent step: the worker's textual result for that step.
pub type StepOutput {
  StepOutput(result: String)
}

/// A reviewer's gate decision.
pub type Decision {
  Approve
  Reject
}

/// The `agent_review` signal: a human's gate decision plus their identity.
pub type ReviewSignal {
  ReviewSignal(decision: Decision, reviewer: String)
}

/// Answer of the `agent_status` query: the live stage for a task.
pub type AgentStatus {
  AgentStatus(stage: String, task_id: String)
}

/// The workflow's recorded result: the disposition plus every step artifact,
/// so a held run is fully inspectable afterwards.
pub type Output {
  Output(
    task_id: String,
    disposition: String,
    scout_finding: String,
    act_artifact: String,
    verify_verdict: String,
    reviewed_by: String,
    reason: String,
  )
}
