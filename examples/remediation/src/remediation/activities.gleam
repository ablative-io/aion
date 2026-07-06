//// The typed activity constructors the remediation workflows dispatch.
////
//// Eight activity types, all served by the remediation worker on ONE distinct
//// task queue (`remediation`) so the run never collides with other workers on
//// `default`:
////
//// - THREE agent activities (`test_author`, `developer`, `verifier`) — driven
////   Norn agents. Their INPUT is the STRUCTURED context (the codec is where
////   the test-author's recommendation strip happens); the worker assembles
////   the final prompt as {role profile markdown} + {this context JSON} and
////   the driven harness returns the schema-constrained structured result.
////   (A fourth role connection, `re_auditor`, is served by the worker for the
////   wave-level Stage 4 to come; no workflow here dispatches it yet.)
//// - FIVE shell activities (`provision_workspace`, `gate1`, `gate2`,
////   `ledger_update`, `cleanup_workspace`) — typed registry handlers running
////   real git/cargo/python commands.
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

fn on_queue(activity: Activity(i, o)) -> Activity(i, o) {
  activity.task_queue(activity, task_queue)
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
  |> on_queue
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
  |> on_queue
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
  |> on_queue
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
  |> on_queue
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
  |> on_queue
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
  |> on_queue
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
  |> on_queue
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
  |> on_queue
}
