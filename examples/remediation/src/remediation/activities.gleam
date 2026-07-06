//// The typed activity constructors the remediation workflows dispatch.
////
//// Eight activity types, all served by the remediation worker on ONE distinct
//// task queue (`remediation`) so the run never collides with other workers on
//// `default`, each PINNED to the one worker connection that serves it via the
//// node routing dimension (namespace × task_queue × node):
////
//// - THREE agent activities (`test_author`, `developer`, `verifier`) — driven
////   Norn agents, each pinned to its role's node. Their INPUT is the
////   STRUCTURED context (the codec is where the test-author's recommendation
////   strip happens); the worker assembles the final prompt as {role profile
////   markdown} + {this context JSON} and the driven harness returns the
////   schema-constrained structured result. (A fourth role connection,
////   `re_auditor` on node `re_auditor_node`, is served by the worker for the
////   wave-level Stage 4 to come; no workflow here dispatches it yet.)
//// - FIVE shell activities (`provision_workspace`, `gate1`, `gate2`,
////   `ledger_update`, `cleanup_workspace`) — typed registry handlers running
////   real git/cargo/python commands, all pinned to the `shell` node.
////
//// The node pin is LOAD-BEARING: the server routes a pushed activity to a
//// worker connection by (namespace, task_queue, node) ONLY — never by
//// activity type. The worker opens FIVE connections on this one task queue,
//// so without the pin an activity round-robins across all five and 4-in-5
//// times lands on a connection with no handler for it, never reports, and
//// dies at the heartbeat window as a lost-worker failure.
////
//// Every activity dispatches on the remote wire; the local runner is a guard
//// that never runs in production and fails loudly if a misconfiguration ever
//// routed one in-VM.

import aion/activity.{type Activity}
import aion/error
import remediation/codecs
import remediation/types.{
  type CleanupInput, type CleanupOutcome, type DeveloperInput, type FixReport,
  type Gate1Input, type Gate1Outcome, type Gate2Input, type Gate2Outcome,
  type LedgerUpdateInput, type LedgerUpdateOutcome, type ProvisionInput,
  type TestAuthorInput, type TestManifest, type Verdict, type VerifierInput,
  type WorkspaceInfo,
}

/// The one task queue every remediation activity is dispatched on.
pub const task_queue = "remediation"

// --- node ids (the routing SOURCE OF TRUTH) -----------------------------------
//
// These five constants are THE authoritative node-id table for the remediation
// flow. The worker (worker/src/main.rs) registers each of its five liminal
// connections with exactly one of these ids: the shell connection registers
// `shell_node`, and each agent connection's node id IS its role's activity
// type (`Role::node()` there returns `activity_type`, which mirrors
// `test_author_node`/`developer_node`/`verifier_node`/`re_auditor_node` here).
// The strings MUST match exactly — the server routes on them blindly, so a
// mismatch strands the activity on a connection that cannot serve it.

/// The node the worker's SHELL connection registers; every shell activity
/// (`provision_workspace`, `gate1`, `gate2`, `ledger_update`,
/// `cleanup_workspace`) pins to it.
pub const shell_node = "shell"

/// The node of the `test_author` driven-agent connection.
pub const test_author_node = "test_author"

/// The node of the `developer` driven-agent connection.
pub const developer_node = "developer"

/// The node of the `verifier` driven-agent connection.
pub const verifier_node = "verifier"

/// The node of the `re_auditor` driven-agent connection. The worker serves it
/// today; the wave-level Stage 4 activity that will dispatch on it is not
/// built yet. It lives here so the node table stays single-sourced.
pub const re_auditor_node = "re_auditor"

/// A local runner for a remote-only activity: it must never execute in-VM, so
/// it fails loudly rather than returning a plausible-looking empty result.
fn remote_only(
  name: String,
) -> fn(input) -> Result(output, error.ActivityError) {
  fn(_input) {
    Error(error.Terminal(
      message: name
        <> " is a remote-only remediation activity and has no in-VM runner",
      details: "",
    ))
  }
}

/// Pin an activity to the remediation queue AND to the one worker connection
/// that serves it. Both dimensions are required: the queue keeps the run off
/// `default`, and the node selects the single connection (of the worker's
/// five on this queue) holding the handler — the server never routes by
/// activity type.
fn route(activity: Activity(i, o), node: String) -> Activity(i, o) {
  activity.node(activity.task_queue(activity, task_queue), node)
}

// --- agent activities ---------------------------------------------------------

/// `test_author`: a driven fail-first test-authoring pass returning the
/// [`TestManifest`]. The input codec carries NO recommendation field.
pub fn test_author(
  input: TestAuthorInput,
) -> Activity(TestAuthorInput, TestManifest) {
  activity.new(
    "test_author",
    input,
    codecs.test_author_input_codec(),
    codecs.test_manifest_codec(),
    remote_only("test_author"),
  )
  |> route(test_author_node)
}

/// `developer`: a driven fix round returning the [`FixReport`]. The harness
/// resumes the per-brief `{workflow_id}-developer` session across cycles, so a
/// loop-back round carries only the new verdict/gate feedback in context.
pub fn developer(input: DeveloperInput) -> Activity(DeveloperInput, FixReport) {
  activity.new(
    "developer",
    input,
    codecs.developer_input_codec(),
    codecs.fix_report_codec(),
    remote_only("developer"),
  )
  |> route(developer_node)
}

/// `verifier`: a driven adversarial verification returning the [`Verdict`].
pub fn verifier(input: VerifierInput) -> Activity(VerifierInput, Verdict) {
  activity.new(
    "verifier",
    input,
    codecs.verifier_input_codec(),
    codecs.verdict_codec(),
    remote_only("verifier"),
  )
  |> route(verifier_node)
}

// --- shell activities ------------------------------------------------------------

/// `provision_workspace`: create the brief's isolated git worktree, branched
/// on the configured base, returning the base commit gate 1 diffs against.
pub fn provision(
  input: ProvisionInput,
) -> Activity(ProvisionInput, WorkspaceInfo) {
  activity.new(
    "provision_workspace",
    input,
    codecs.provision_input_codec(),
    codecs.workspace_info_codec(),
    remote_only("provision_workspace"),
  )
  |> route(shell_node)
}

/// `gate1`: re-run each authored test; every one must FAIL on the unfixed
/// code. A test that passes (or a dirty worktree) is a recorded FAIL verdict,
/// never an activity error.
pub fn gate1(input: Gate1Input) -> Activity(Gate1Input, Gate1Outcome) {
  activity.new(
    "gate1",
    input,
    codecs.gate1_input_codec(),
    codecs.gate1_outcome_codec(),
    remote_only("gate1"),
  )
  |> route(shell_node)
}

/// `gate2`: the mechanical fix gate — authored-test-path diff empty, clippy
/// `-D warnings` green, full suite green — plus the diff the verifier reads.
pub fn gate2(input: Gate2Input) -> Activity(Gate2Input, Gate2Outcome) {
  activity.new(
    "gate2",
    input,
    codecs.gate2_input_codec(),
    codecs.gate2_outcome_codec(),
    remote_only("gate2"),
  )
  |> route(shell_node)
}

/// `ledger_update`: apply one stage artifact to the in-repo ledger via the
/// yggdrasil applier CLI (`scripts/remediation/apply_transitions.py`).
pub fn ledger_update(
  input: LedgerUpdateInput,
) -> Activity(LedgerUpdateInput, LedgerUpdateOutcome) {
  activity.new(
    "ledger_update",
    input,
    codecs.ledger_update_input_codec(),
    codecs.ledger_update_outcome_codec(),
    remote_only("ledger_update"),
  )
  |> route(shell_node)
}

/// `cleanup_workspace`: remove the brief's worktree, refusing (honestly, as
/// recorded data) when it holds uncommitted work.
pub fn cleanup(input: CleanupInput) -> Activity(CleanupInput, CleanupOutcome) {
  activity.new(
    "cleanup_workspace",
    input,
    codecs.cleanup_input_codec(),
    codecs.cleanup_outcome_codec(),
    remote_only("cleanup_workspace"),
  )
  |> route(shell_node)
}
