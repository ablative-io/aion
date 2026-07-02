//// Typed activity constructors for the agent-dev pipeline.
////
//// Every activity here is served by the agent-dev worker in production. The
//// scout/dev/review agent activities speak the norn-harness contract: ONE
//// prompt string in, ONE terminal-text string out — the workflow composes
//// every prompt and decodes the review verdict defensively
//// (`agent_dev/verdict`). The worker pins one norn session per role per run,
//// so a resume round's lean feedback-only prompt lands in the same session
//// that did the original work.
////
//// The local implementations carried by each `activity.new` are terminal
//// errors on purpose: this package ships no second, in-process
//// implementation of the worker. The `aion/testing` suites register their
//// own scenario handlers with `testing.mock_activity`; a dispatch that
//// reaches one of these stubs is a test that forgot to register a handler,
//// and it fails loudly saying so.

import agent_dev_codecs as codecs
import agent_dev_io as io
import aion/activity
import aion/codec
import aion/duration
import aion/error
import gleam/dynamic/decode
import gleam/json

/// The retry policy every step in this pipeline carries for RETRYABLE
/// failures (worker death/restart mid-step, a lost connection): a few fixed
/// re-deliveries. Safe for the agent steps because the worker pins one norn
/// session per role per run (`--resume-if-exists`): a retried step resumes
/// the SAME session and its accumulated work, never starting over. Terminal
/// failures (a real harness error) are not retried.
fn survives_worker_restart(
  step: activity.Activity(i, o),
) -> activity.Activity(i, o) {
  activity.retry(
    step,
    activity.RetryPolicy(
      max_attempts: 3,
      backoff: activity.Fixed(delay: duration.seconds(5)),
    ),
  )
}

/// Provision the workspace: check out `repo_url` at `base_ref` into a fresh
/// working branch for the brief.
pub fn provision(
  input: io.ProvisionInput,
) -> activity.Activity(io.ProvisionInput, io.Workspace) {
  activity.new(
    "provision",
    input,
    codecs.provision_input_codec(),
    codecs.workspace_codec(),
    unserved("provision"),
  )
  |> survives_worker_restart
}

/// The scout agent step: a read-only research pass over the workspace that
/// returns an implementation plan as terminal text.
pub fn scout(prompt: String) -> activity.Activity(String, String) {
  agent_step("scout", prompt)
}

/// The dev agent step: implements (round one) or applies feedback (resume
/// rounds) in the workspace, returning a work report as terminal text.
pub fn dev(prompt: String) -> activity.Activity(String, String) {
  agent_step("dev", prompt)
}

/// The review agent step: adversarially reviews the work against the
/// contract, ending its terminal text with the JSON verdict the workflow
/// decodes defensively.
pub fn review(prompt: String) -> activity.Activity(String, String) {
  agent_step("review", prompt)
}

/// Run the authoritative checks in the workspace. A failing gate is recorded
/// data (`pass: False` plus diagnostics), never an activity error.
pub fn gate(
  workspace: io.Workspace,
) -> activity.Activity(io.Workspace, io.GateDetail) {
  activity.new(
    "gate",
    workspace,
    codecs.workspace_codec(),
    codecs.gate_detail_codec(),
    unserved("gate"),
  )
  |> survives_worker_restart
}

/// Merge the workspace branch. Dispatched ONLY on a `Passed` disposition.
pub fn land(
  input: io.LandInput,
) -> activity.Activity(io.LandInput, io.LandOutput) {
  activity.new(
    "land",
    input,
    codecs.land_input_codec(),
    codecs.land_output_codec(),
    unserved("land"),
  )
}

/// One agent step under the norn-harness contract: prompt in, terminal text
/// out, both plain JSON strings on the wire.
fn agent_step(
  role: String,
  prompt: String,
) -> activity.Activity(String, String) {
  activity.new(role, prompt, text_codec(), text_codec(), unserved(role))
  |> survives_worker_restart
}

/// The wire codec for agent prompts and terminal text: a bare JSON string.
pub fn text_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

/// The deliberate local stub: production serves this name from the agent-dev
/// worker, and tests must register a scenario handler for it.
fn unserved(name: String) -> fn(input) -> Result(output, error.ActivityError) {
  fn(_input) {
    Error(error.terminal(
      "the `"
      <> name
      <> "` activity is served by the agent-dev worker; register a "
      <> "testing.mock_activity handler for it in tests",
    ))
  }
}
