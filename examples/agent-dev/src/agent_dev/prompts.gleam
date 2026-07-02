//// Prompt composition for the agent-dev pipeline — pure functions only.
////
//// The workflow composes EVERY prompt; the worker adds nothing. Round-one
//// prompts carry the full contract (brief, design notes, acceptance
//// criteria, and — for dev — the scout plan). Resume prompts are lean and
//// feedback-only, because the worker pins one norn session per role per
//// run: the session already holds the contract from its first prompt, and
//// repeating it would bloat every round.
////
//// The review prompts end with `verdict_instruction`, the exact JSON-object
//// contract `agent_dev/verdict` decodes defensively.

import agent_dev_io as io
import gleam/list
import gleam/string

/// The in-prompt verdict contract: the reviewer must END its reply with
/// exactly this JSON object. `agent_dev/verdict.parse` extracts it from the
/// tail of the terminal text.
pub const verdict_instruction = "End your reply with exactly this JSON object as the final text, on its own line:\n{\"pass\": <true|false>, \"blockers\": [\"<blocker>\", ...], \"summary\": \"<one-paragraph summary>\"}\nSet \"pass\" to true ONLY if there are zero blockers and the work is production-ready."

/// The bounded re-ask sent when the reviewer's terminal text carried no
/// parseable verdict: one more chance, JSON only.
pub const verdict_reask = "Your previous reply did not end with a parseable verdict. Respond with only the JSON verdict — no prose, no code fences:\n{\"pass\": <true|false>, \"blockers\": [\"<blocker>\", ...], \"summary\": \"<one-paragraph summary>\"}"

/// Round-one scout prompt: the full contract, asking for a concrete
/// implementation plan without changing anything.
pub fn scout(input: io.Input) -> String {
  "You are the scout for brief "
  <> input.brief_id
  <> ". Research the repository and produce a concrete implementation plan. "
  <> "Do NOT change any file: read, then plan.\n\n"
  <> contract(input)
  <> "\n\nReport: the files involved, the approach you recommend, and the "
  <> "risks the developer must watch."
}

/// Round-one dev prompt: the full contract plus the scout's plan.
pub fn dev_start(input: io.Input, plan: String) -> String {
  "You are the developer for brief "
  <> input.brief_id
  <> ". Implement the brief in this workspace, then report exactly what "
  <> "you changed and why.\n\n"
  <> contract(input)
  <> "\n\n## Scout plan\n"
  <> plan
}

/// Lean dev resume prompt for a failing review: the blockers and the
/// reviewer's summary, nothing else — the pinned session holds the contract.
pub fn dev_review_feedback(verdict: io.ReviewVerdict) -> String {
  "The review found blockers. Address every one, then report your changes."
  <> "\n\n## Blockers\n"
  <> bullet_list(verdict.blockers)
  <> "\n\n## Reviewer summary\n"
  <> verdict.summary
}

/// Lean dev resume prompt for a failing gate: the diagnostics, nothing else.
pub fn dev_gate_feedback(diagnostics: String) -> String {
  "The gate failed. Fix every reported problem, then report your changes."
  <> "\n\n## Gate diagnostics\n"
  <> diagnostics
}

/// Round-one review prompt: the full contract, the dev report, and the
/// verdict contract.
pub fn review_start(input: io.Input, dev_report: String) -> String {
  "You are the adversarial reviewer for brief "
  <> input.brief_id
  <> ". Review the work in this workspace against the contract below — "
  <> "the contract, not merely the diff. Require evidence for every "
  <> "blocker.\n\n"
  <> contract(input)
  <> "\n\n## Dev report\n"
  <> dev_report
  <> "\n\n"
  <> verdict_instruction
}

/// Lean review resume prompt: the new dev report and the verdict contract —
/// the pinned session holds the contract from round one.
pub fn review_resume(dev_report: String) -> String {
  "The developer has revised the work. Re-review it against the same "
  <> "contract.\n\n## Dev report\n"
  <> dev_report
  <> "\n\n"
  <> verdict_instruction
}

/// The shared contract block: brief, design notes, acceptance criteria.
fn contract(input: io.Input) -> String {
  "## Brief "
  <> input.brief_id
  <> "\n"
  <> input.brief
  <> "\n\n## Design notes\n"
  <> input.design_notes
  <> "\n\n## Acceptance criteria\n"
  <> bullet_list(input.acceptance)
}

fn bullet_list(items: List(String)) -> String {
  case items {
    [] -> "(none listed)"
    _ ->
      items
      |> list.map(fn(item) { "- " <> item })
      |> string.join("\n")
  }
}
