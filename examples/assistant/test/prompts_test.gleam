//// Unit tests for prompt composition: the round-one prompt carries the full
//// working contract (ground truth, authoring method, honesty rules, the
//// workspace and its confinement note); continuation prompts are the
//// operator's message verbatim (the pinned norn session already holds the
//// contract from round one).

import assistant/prompts
import assistant_io as io
import gleam/string
import gleeunit/should
import support/harness

fn contains(haystack: String, needle: String) {
  haystack |> string.contains(needle) |> should.be_true
}

fn workspace() -> io.Workspace {
  io.Workspace(path: "/clones/run-1/repo")
}

pub fn first_round_carries_the_objective_and_workspace_test() {
  let input = harness.base_input()
  let prompt = prompts.first_round(input, workspace())
  contains(prompt, input.objective)
  contains(prompt, "/clones/run-1/repo")
  contains(prompt, input.repo_path)
}

pub fn first_round_teaches_ground_truth_over_memory_test() {
  let prompt = prompts.first_round(harness.base_input(), workspace())
  contains(prompt, "from memory")
  contains(prompt, ".assistant/resources/")
  contains(prompt, "ENVIRONMENT.md")
  contains(prompt, "SCAFFOLD.md")
  contains(prompt, "docs/GETTING-STARTED.md")
  contains(prompt, "docs/guides/")
  contains(prompt, "gleam/aion_flow/src/aion/")
}

pub fn first_round_teaches_the_types_first_authoring_canon_test() {
  let prompt = prompts.first_round(harness.base_input(), workspace())
  contains(prompt, "examples/hello-world")
  contains(prompt, "examples/agent-dev")
  contains(prompt, "aion generate")
  contains(prompt, "NEVER edit or format generated files")
  contains(prompt, "workflow.toml")
}

pub fn first_round_teaches_build_and_deploy_concretely_test() {
  let prompt = prompts.first_round(harness.base_input(), workspace())
  contains(prompt, "package <dir> --build")
  contains(prompt, ".aion")
  contains(prompt, "deploy")
  contains(prompt, "start")
}

pub fn first_round_carries_the_honesty_rules_test() {
  let prompt = prompts.first_round(harness.base_input(), workspace())
  contains(prompt, "I have not verified this")
  contains(prompt, "Never invent SDK surface")
}

pub fn first_round_states_the_file_tool_confinement_test() {
  // The norn file tools are confined to the workspace; shell commands start
  // there but can read elsewhere — the prompt must say so.
  let prompt = prompts.first_round(harness.base_input(), workspace())
  contains(prompt, "confined")
  contains(prompt, "shell")
}

pub fn first_round_without_a_repo_self_serves_a_clone_test() {
  let input = io.Input(objective: "explain signals", repo_path: "")
  let prompt = prompts.first_round(input, workspace())
  contains(prompt, "scratch git workspace")
  contains(prompt, "No repository was attached")
  contains(
    prompt,
    "git clone --depth 1 https://github.com/ablative-io/aion.git",
  )
  contains(prompt, "private")
  contains(prompt, ".assistant/resources/")
}

pub fn continuation_is_the_operator_message_verbatim_test() {
  prompts.continuation("Make the timeout configurable, please")
  |> should.equal("Make the timeout configurable, please")
}
