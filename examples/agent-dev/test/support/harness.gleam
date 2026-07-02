//// Hermetic-test seam for the agent-dev pipeline.
////
//// Each scenario registers one typed handler per activity through
//// `aion/testing.mock_activity` — the same names, codecs, and dispatch path
//// the deployed workflow uses, with the test's handlers standing where the
//// agent-dev worker stands in production. `passing/0` is the all-green
//// baseline; scenarios override individual handlers with a record update.
////
//// The default dev handler ECHOES its prompt into the dev report, so a
//// review handler can observe (statelessly) whether its feedback made the
//// round trip through the dev session — the seam the review-fail scenario
//// keys on. `counter_next` (test FFI, process-dictionary scoped) is the
//// seam for handlers that must vary by call number, like a gate that fails
//// once then passes.

import agent_dev/activities
import agent_dev_io as io
import aion/error
import aion/testing

/// One handler per activity the pipeline dispatches.
pub type Handlers {
  Handlers(
    provision: fn(io.ProvisionInput) ->
      Result(io.Workspace, error.ActivityError),
    scout: fn(String) -> Result(String, error.ActivityError),
    dev: fn(String) -> Result(String, error.ActivityError),
    review: fn(String) -> Result(String, error.ActivityError),
    gate: fn(io.Workspace) -> Result(io.GateDetail, error.ActivityError),
    land: fn(io.LandInput) -> Result(io.LandOutput, error.ActivityError),
  )
}

/// Per-process invocation counter (1 on the first call for a key). The
/// harness runs handlers in the test's own process, so this is test-scoped.
@external(erlang, "agent_dev_test_ffi", "counter_next")
pub fn counter_next(key: String) -> Int

/// A reviewer's passing terminal text: prose first, the JSON verdict at the
/// end — exactly what the in-prompt contract asks for.
pub const pass_verdict_text = "The work is complete and production-ready. No blockers found.\n{\"pass\": true, \"blockers\": [], \"summary\": \"all acceptance criteria met\"}"

/// A reviewer's failing terminal text carrying one blocker.
pub fn fail_verdict_text(blocker: String) -> String {
  "Blockers were found.\n{\"pass\": false, \"blockers\": [\""
  <> blocker
  <> "\"], \"summary\": \"changes required\"}"
}

/// The workflow input every scenario starts from. Both caps are required
/// fields, so each test states them explicitly.
pub fn base_input() -> io.Input {
  io.Input(
    repo_url: "/repos/chiron",
    base_ref: "main",
    brief_id: "CHIRON-RUFF-001",
    brief: "Add the missing ruff diagnostics adapter.",
    design_notes: "biome.rs is the compiled reference; adapter/declarative is the TOML style.",
    acceptance: [
      "ruff JSON output parses into DiagnosticEvents",
      "the adapter is registered for *.py patterns",
    ],
    dev_review_cap: 5,
    gate_cap: 3,
  )
}

/// The all-green baseline: provisioning succeeds deterministically, the
/// scout plans, the dev echoes its prompt into the report, the review
/// passes, the gate passes, the land commits.
pub fn passing() -> Handlers {
  Handlers(
    provision: fn(input) {
      Ok(io.Workspace(
        path: "/work/" <> input.brief_id,
        branch: "agent-dev/" <> input.brief_id,
      ))
    },
    scout: fn(_prompt) {
      Ok("PLAN: add the ruff adapter beside the biome adapter")
    },
    dev: fn(prompt) { Ok("DEV-REPORT\n" <> prompt) },
    review: fn(_prompt) { Ok(pass_verdict_text) },
    gate: fn(_workspace) { Ok(io.GateDetail(pass: True, diagnostics: "")) },
    land: fn(_input) { Ok(io.LandOutput(commit_sha: "cafe1234")) },
  )
}

/// A handler for an activity the scenario asserts NEVER runs: reaching it
/// fails the dispatch loudly, which fails the test through the workflow's
/// typed stage error.
pub fn must_not_run(
  name: String,
) -> fn(input) -> Result(output, error.ActivityError) {
  fn(_input) {
    Error(error.terminal(
      "the `" <> name <> "` activity must not run in this scenario",
    ))
  }
}

/// Fresh harness env with every activity's scenario handler registered.
pub fn register(handlers: Handlers) -> Nil {
  let assert Ok(env) = testing.new()
  let workspace = io.Workspace(path: "", branch: "")
  let assert Ok(_) =
    testing.mock_activity(
      env,
      activities.provision(io.ProvisionInput(
        repo_url: "",
        base_ref: "",
        brief_id: "",
        run_id: "",
      )),
      handlers.provision,
    )
  let assert Ok(_) =
    testing.mock_activity(env, activities.scout(""), handlers.scout)
  let assert Ok(_) =
    testing.mock_activity(env, activities.dev(""), handlers.dev)
  let assert Ok(_) =
    testing.mock_activity(env, activities.review(""), handlers.review)
  let assert Ok(_) =
    testing.mock_activity(env, activities.gate(workspace), handlers.gate)
  let assert Ok(_) =
    testing.mock_activity(
      env,
      activities.land(io.LandInput(workspace: workspace, brief_id: "")),
      handlers.land,
    )
  Nil
}
