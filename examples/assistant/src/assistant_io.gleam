//// Boundary types for the assistant workflow — the authored source of truth
//// (ADR-014, types-first).
////
//// Declare types only here: `aion generate` derives the JSON codecs
//// (`src/assistant_codecs.gleam`) and the emitted `schemas/*.json` artifacts
//// from these types. Edit a type, run `aion generate`, and commit the type
//// with its regenerated artifacts together.
////
//// The `assistant` agent activity is deliberately absent from this module:
//// its wire contract is one prompt string in, one terminal-text string out
//// (the norn-harness contract), carried by a plain string codec in
//// `assistant/activities` — there is no record shape to declare.

import gleam/option

/// The workflow's typed error: a stage that could not execute at all (a
/// failed activity dispatch or a broken operator-signal channel, never a
/// session the operator chose to end — that is a recorded disposition).
pub type AssistantError {
  AssistantError(stage: String, message: String)
}

/// Answer of the `assistant_status` query: the live phase (`provisioning`,
/// `working`, `awaiting_operator`) and the current round.
pub type AssistantStatus {
  AssistantStatus(phase: String, round: Int)
}

/// Payload of the `assistant_continue` signal — the ONE control signal the
/// session listens on (the engine's selective receive wakes on exactly one
/// signal name, so continue and end share the name and discriminate here).
/// Both fields are optional on the wire: `{"message": "..."}` continues the
/// session with the operator's message; `{"end": true}` ends it cleanly.
pub type Continuation {
  Continuation(message: option.Option(String), end: option.Option(Bool))
}

/// The session's terminal disposition. Both are clean completions: the
/// operator ended the session, or the bounded round budget was spent.
pub type Disposition {
  OperatorEnded
  RoundCapExhausted
}

/// The workflow's start input. `objective` is the operator's opening ask
/// (must be non-empty); `repo_path` is the aion repository the assistant
/// grounds its answers in (a local path or clone URL) — empty means no repo,
/// so the session gets a scratch workspace and the assistant must say what it
/// cannot verify.
pub type Input {
  Input(objective: String, repo_path: String)
}

/// The workflow's recorded result: how the session ended, how many agent
/// rounds ran, the final agent reply, and the workspace path (which persists
/// for inspection).
pub type Output {
  Output(
    disposition: Disposition,
    rounds: Int,
    last_reply: String,
    workspace_path: String,
  )
}

/// Input of the `assistant_provision` activity: materialise the session
/// workspace at `<root>/<run_id>/repo` — a clone of `repo_path` when given,
/// a fresh scratch git workspace when empty.
///
/// `run_id` is the workflow's run id (from `workflow.id()`): it keys the
/// worker-side workspace directory AND must equal the id the agent-harness
/// workspace-root template expands — the same value (#175).
pub type ProvisionInput {
  ProvisionInput(repo_path: String, run_id: String)
}

/// The provisioned session workspace every assistant round works inside.
pub type Workspace {
  Workspace(path: String)
}
