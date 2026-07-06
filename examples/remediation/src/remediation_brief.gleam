//// The CHILD workflow: one remediation brief, end-to-end (DESIGN.md stages
//// 1-3 + the mechanical stage-5 slice), run as its own linked execution.
////
//// Why a child workflow per brief (the pipeline-run pattern, exactly): DRIVEN
//// Norn sessions are keyed by a session id the harness builds from
//// `{workflow_id}`. A brief's test-author, developer, and verifier must each
//// keep ONE session resumed across that brief's cycles, and two briefs must
//// never share a session. A child workflow gives each brief its own
//// `{workflow_id}`, so `{workflow_id}-developer` etc. are automatically
//// per-brief AND stable across cycles — the brief's identity materialized in
//// the child's own workflow id (and in its branch name,
//// `remediation/<brief-id>`). It also makes briefs in a stratum genuinely
//// parallel: the parent spawns them concurrently.
////
//// The body:
////   provision an isolated worktree (branch `remediation/<brief-id>` on the
////   configured base)
////   -> test_author (AGENT; input codec-stripped of every recommendation)
////   -> coverage check (pure: every correction has a test or an explicit
////      could_not_reproduce)
////   -> gate1 (SHELL: re-run the authored tests — each must FAIL; committed)
////   -> the bounded fix cycle (`remediation/cycle`) as a trampoline:
////        developer (AGENT; full entries incl. recommendation)
////        -> gate2 (SHELL: authored-test diff empty, clippy green, suite
////           green) — a red gate loops back to the developer
////        -> verifier (AGENT; per-finding rulings) — any adverse ruling loops
////           back to the developer with the verdict attached
////      cycle-capped; exhaustion is a TERMINAL DISPOSITION recorded in the
////      ledger, never a silent success
////   -> ledger_update (SHELL, once per artifact: test_manifest, fix_report,
////      verdict, disposition)
////   -> cleanup (SHELL: remove the worktree; the branch and its commits
////      remain; a dirty worktree is left in place).
////
//// `could_not_reproduce` findings are carried through to the [`BriefResult`]
//// for the operator (DECISIONS.md D4 — no automated reroute).

import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/string
import remediation/activities
import remediation/checks
import remediation/codecs
import remediation/cycle
import remediation/types.{
  type BriefInput, type BriefResult, type Disposition, type FixReport,
  type Gate1Outcome, type Gate2Outcome, type LedgerApplication,
  type RemediationError, type TestManifest, type Verdict, type WorkspaceInfo,
  Accepted, BriefResult, CleanupInput, CycleCapExhausted, DeveloperInput,
  Gate1Failed, Gate1Input, Gate2Input, LedgerApplication, LedgerUpdateInput,
  ProvisionInput, StageFailed, TestAuthorInput, VerifierInput,
}

/// The parent and child agree on this base directory for brief workspaces:
/// each brief's worktree lives at `<base>/<child_workflow_id>`. The Rust
/// worker's test-author/developer/verifier harnesses point Norn's
/// `--workspace-root` at the SAME `<base>/{workflow_id}` template, so a driven
/// agent operates in exactly the worktree the `provision_workspace` activity
/// created for its brief. Keep this in sync with `WORKSPACE_BASE` in
/// `worker/src/handlers.rs`.
pub const workspace_base = "/tmp/aion-remediation/ws"

/// Typed definition binding the codecs to the child execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  BriefInput,
  BriefResult,
  RemediationError,
) {
  workflow.define(
    "remediation_brief",
    codecs.brief_input_codec(),
    codecs.brief_result_codec(),
    codecs.remediation_error_codec(),
    execute,
  )
}

/// Engine entry point for the child workflow.
pub fn run(raw_input: Dynamic) -> Result(String, RemediationError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.brief_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(result) -> Ok(codecs.brief_result_codec().encode(result))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(types.DecodeInputFailed(
            "failed to decode brief input: " <> reason,
          ))
      }
    Error(_) ->
      Error(types.DecodeInputFailed("brief input payload was not a string"))
  }
}

/// The child body: provision, author, gate 1, then the bounded fix cycle,
/// then the mechanical ledger/cleanup tail.
pub fn execute(input: BriefInput) -> Result(BriefResult, RemediationError) {
  let cap =
    cycle.resolve_cap(
      input.config.max_fix_cycles,
      types.default_max_fix_cycles(),
    )

  use workspace <- try(provision(input))
  use manifest <- try(run_test_author(input))

  // Gate 1, workflow half (pure): every correction finding must have a test
  // or an explicit could_not_reproduce flag — a missing one is a silently
  // dropped finding, which terminates the brief as a recorded gate-1 failure.
  case checks.uncovered_corrections(input.entries, manifest) {
    [] -> after_coverage(input, cap, workspace, manifest)
    uncovered ->
      finalize(
        input,
        workspace,
        manifest,
        fix_report: None,
        verdict: None,
        disposition: Gate1Failed,
        fix_cycles: 0,
        test_edit_attempts: 0,
        detail: "correction findings without an authored test or a "
          <> "could_not_reproduce flag: "
          <> string.join(uncovered, ", "),
      )
  }
}

fn after_coverage(
  input: BriefInput,
  cap: Int,
  workspace: WorkspaceInfo,
  manifest: TestManifest,
) -> Result(BriefResult, RemediationError) {
  // Gate 1, shell half: re-run every authored test; each must FAIL (evidence
  // is verified, not trusted — DESIGN.md Gate 1).
  use gate1 <- try(run_gate1(workspace, manifest))
  case gate1.pass {
    False ->
      finalize(
        input,
        workspace,
        manifest,
        fix_report: None,
        verdict: None,
        disposition: Gate1Failed,
        fix_cycles: 0,
        test_edit_attempts: 0,
        detail: "gate 1 failed: " <> gate1.detail,
      )
    True -> {
      let state =
        LoopState(
          workspace: workspace,
          manifest: manifest,
          gate1: gate1,
          fix_report: None,
          last_gate2: None,
          verdict: None,
          test_edit_attempts: 0,
        )
      drive(input, cycle.initial(cap), state)
    }
  }
}

/// The carried artifacts alongside the pure cap machine: the workspace and
/// the most recent developer/gate/verifier results, used to compose the next
/// activity input and to build the terminal [`BriefResult`].
type LoopState {
  LoopState(
    workspace: WorkspaceInfo,
    manifest: TestManifest,
    gate1: Gate1Outcome,
    fix_report: Option(FixReport),
    last_gate2: Option(Gate2Outcome),
    verdict: Option(Verdict),
    test_edit_attempts: Int,
  )
}

/// The trampoline: ask the machine for the next instruction, perform exactly
/// that one effect, fold the outcome back, recurse.
fn drive(
  input: BriefInput,
  machine: cycle.Machine,
  state: LoopState,
) -> Result(BriefResult, RemediationError) {
  case cycle.plan(machine) {
    cycle.Stop(disposition) ->
      finalize(
        input,
        state.workspace,
        state.manifest,
        fix_report: state.fix_report,
        verdict: state.verdict,
        disposition: disposition,
        fix_cycles: machine.fix_rounds,
        test_edit_attempts: state.test_edit_attempts,
        detail: stop_detail(disposition, state),
      )
    cycle.Developer -> {
      use report <- try(run_developer(input, state))
      drive(
        input,
        cycle.on_developer(machine),
        LoopState(..state, fix_report: Some(report)),
      )
    }
    cycle.Gate2 -> {
      use outcome <- try(run_gate2(state))
      let attempts = case outcome.test_diff_clean {
        True -> state.test_edit_attempts
        // An authored-test edit reached the gate: a guard-failure metric,
        // counted per occurrence (DESIGN.md fix metrics).
        False -> state.test_edit_attempts + 1
      }
      drive(
        input,
        cycle.on_gate2(machine, outcome.pass),
        LoopState(
          ..state,
          last_gate2: Some(outcome),
          test_edit_attempts: attempts,
        ),
      )
    }
    cycle.Verifier -> {
      use verdict <- try(run_verifier(input, state))
      drive(
        input,
        cycle.on_verdict(machine, checks.verdict_accepts(verdict)),
        LoopState(..state, verdict: Some(verdict)),
      )
    }
  }
}

// --- effects ------------------------------------------------------------------

fn provision(input: BriefInput) -> Result(WorkspaceInfo, RemediationError) {
  use child_id <- try(engine_id())
  let workspace_path = workspace_base <> "/" <> child_id
  let branch = "remediation/" <> checks.branch_safe(input.brief.id)
  case
    workflow.run(
      activities.provision(ProvisionInput(
        repo_root: input.config.repo_root,
        base_branch: input.config.base_branch,
        branch: branch,
        workspace_path: workspace_path,
      )),
    )
  {
    Ok(info) -> Ok(info)
    Error(activity_error) -> stage_error("provision_workspace", activity_error)
  }
}

fn run_test_author(
  input: BriefInput,
) -> Result(TestManifest, RemediationError) {
  // THE INDEPENDENCE BOUNDARY: entries are projected through
  // strip_recommendation into a type with no recommendation field before they
  // touch the activity input codec, so the recommendation never reaches the
  // wire (test/codec_test pins this).
  let stripped = list.map(input.entries, types.strip_recommendation)
  case
    workflow.run(
      activities.test_author(TestAuthorInput(
        brief: input.brief,
        entries: stripped,
      )),
    )
  {
    Ok(manifest) -> Ok(manifest)
    Error(activity_error) -> stage_error("test_author", activity_error)
  }
}

fn run_gate1(
  workspace: WorkspaceInfo,
  manifest: TestManifest,
) -> Result(Gate1Outcome, RemediationError) {
  case
    workflow.run(
      activities.gate1(Gate1Input(
        workspace_path: workspace.workspace_path,
        base_commit: workspace.base_commit,
        tests: checks.runnable_tests(manifest),
      )),
    )
  {
    Ok(outcome) -> Ok(outcome)
    Error(activity_error) -> stage_error("gate1", activity_error)
  }
}

fn run_developer(
  input: BriefInput,
  state: LoopState,
) -> Result(FixReport, RemediationError) {
  case
    workflow.run(
      activities.developer(DeveloperInput(
        brief: input.brief,
        entries: input.entries,
        manifest: state.manifest,
        gate1_results: state.gate1.results,
        verdict: state.verdict,
        gate2: state.last_gate2,
      )),
    )
  {
    Ok(report) -> Ok(report)
    Error(activity_error) -> stage_error("developer", activity_error)
  }
}

fn run_gate2(state: LoopState) -> Result(Gate2Outcome, RemediationError) {
  case
    workflow.run(
      activities.gate2(Gate2Input(
        workspace_path: state.workspace.workspace_path,
        tests_commit: state.gate1.tests_commit,
        authored_test_paths: state.gate1.authored_test_paths,
      )),
    )
  {
    Ok(outcome) -> Ok(outcome)
    Error(activity_error) -> stage_error("gate2", activity_error)
  }
}

fn run_verifier(
  input: BriefInput,
  state: LoopState,
) -> Result(Verdict, RemediationError) {
  // The verifier's declared inputs (DESIGN.md Stage 3): original findings,
  // the developer's diff (from gate 2), the fix report, the test manifest.
  // Both artifacts exist whenever the machine reaches the verifier: the
  // developer and gate 2 precede it on every path. Their absence is an
  // engine-ordering fault surfaced loudly, never defaulted around.
  case state.fix_report, state.last_gate2 {
    Some(report), Some(gate2) ->
      case
        workflow.run(
          activities.verifier(VerifierInput(
            brief: input.brief,
            entries: input.entries,
            manifest: state.manifest,
            fix_report: report,
            diff: gate2.diff,
          )),
        )
      {
        Ok(verdict) -> Ok(verdict)
        Error(activity_error) -> stage_error("verifier", activity_error)
      }
    _, _ ->
      Error(StageFailed(
        stage: "verifier",
        message: "verifier reached without a fix report and a gate-2 outcome"
          <> " — cycle-machine ordering violated",
      ))
  }
}

// --- the mechanical tail: ledger + cleanup + result ------------------------------

/// Apply every produced artifact to the ledger (a status cannot change
/// without its artifact — DESIGN.md Stage 5), remove the worktree, and build
/// the terminal result. Applier refusals are RECORDED on the result, never
/// swallowed.
fn finalize(
  input: BriefInput,
  workspace: WorkspaceInfo,
  manifest: TestManifest,
  fix_report fix_report: Option(FixReport),
  verdict verdict: Option(Verdict),
  disposition disposition: Disposition,
  fix_cycles fix_cycles: Int,
  test_edit_attempts test_edit_attempts: Int,
  detail detail: String,
) -> Result(BriefResult, RemediationError) {
  let could_not_reproduce = checks.could_not_reproduce_ids(manifest)

  let artifacts =
    [
      Some(#(
        types.TestManifestArtifact,
        codecs.test_manifest_artifact_json(manifest),
      )),
      option.map(fix_report, fn(report) {
        #(types.FixReportArtifact, codecs.fix_report_artifact_json(report))
      }),
      option.map(verdict, fn(the_verdict) {
        #(types.VerdictArtifact, codecs.verdict_artifact_json(the_verdict))
      }),
      Some(#(
        types.DispositionArtifact,
        codecs.disposition_artifact_json(
          brief_id: input.brief.id,
          disposition: disposition,
          fix_cycles: fix_cycles,
          test_edit_attempts: test_edit_attempts,
          could_not_reproduce: could_not_reproduce,
          detail: detail,
        ),
      )),
    ]
    |> option.values

  use ledger <- try(apply_artifacts(input, artifacts, []))
  use cleanup <- try(run_cleanup(input, workspace))

  let first_pass_accepted = disposition == Accepted && fix_cycles == 1
  Ok(BriefResult(
    brief_id: input.brief.id,
    disposition: disposition,
    fix_cycles: fix_cycles,
    first_pass_accepted: first_pass_accepted,
    could_not_reproduce: could_not_reproduce,
    test_edit_attempts: test_edit_attempts,
    branch: workspace.branch,
    manifest: manifest,
    fix_report: fix_report,
    verdict: verdict,
    ledger: ledger,
    workspace_removed: cleanup.removed,
    summary: brief_summary(input, disposition, fix_cycles, detail, ledger),
  ))
}

fn apply_artifacts(
  input: BriefInput,
  artifacts: List(#(types.ArtifactKind, String)),
  acc: List(LedgerApplication),
) -> Result(List(LedgerApplication), RemediationError) {
  case artifacts {
    [] -> Ok(list.reverse(acc))
    [#(kind, artifact_json), ..rest] ->
      case
        workflow.run(
          activities.ledger_update(LedgerUpdateInput(
            repo_root: input.config.repo_root,
            ledger_path: input.config.ledger_path,
            kind: kind,
            artifact_json: artifact_json,
          )),
        )
      {
        Ok(outcome) ->
          apply_artifacts(input, rest, [
            LedgerApplication(
              kind: types.artifact_kind_to_string(kind),
              applied: outcome.applied,
              detail: outcome.detail,
            ),
            ..acc
          ])
        Error(activity_error) -> stage_error("ledger_update", activity_error)
      }
  }
}

fn run_cleanup(
  input: BriefInput,
  workspace: WorkspaceInfo,
) -> Result(types.CleanupOutcome, RemediationError) {
  case
    workflow.run(
      activities.cleanup(CleanupInput(
        repo_root: input.config.repo_root,
        workspace_path: workspace.workspace_path,
      )),
    )
  {
    Ok(outcome) -> Ok(outcome)
    Error(activity_error) -> stage_error("cleanup_workspace", activity_error)
  }
}

// --- summaries -------------------------------------------------------------------

fn stop_detail(disposition: Disposition, state: LoopState) -> String {
  case disposition {
    Accepted -> "every ruling fixed"
    CycleCapExhausted ->
      "fix-cycle budget exhausted; last adverse evidence: "
      <> last_adverse_evidence(state)
    Gate1Failed -> "gate 1 failed"
  }
}

fn last_adverse_evidence(state: LoopState) -> String {
  case state.verdict {
    Some(verdict) -> string.join(checks.adverse_rulings(verdict), "; ")
    None ->
      case state.last_gate2 {
        Some(gate2) -> "gate 2 red: " <> gate2.diagnostics
        None -> "no gate or verdict evidence recorded"
      }
  }
}

fn brief_summary(
  input: BriefInput,
  disposition: Disposition,
  fix_cycles: Int,
  detail: String,
  ledger: List(LedgerApplication),
) -> String {
  let unapplied =
    ledger
    |> list.filter(fn(application) { !application.applied })
    |> list.map(fn(application) { application.kind })
  let ledger_note = case unapplied {
    [] -> "ledger updated"
    kinds -> "LEDGER NOT FULLY APPLIED (" <> string.join(kinds, ", ") <> ")"
  }
  "Brief "
  <> input.brief.id
  <> ": "
  <> types.disposition_to_string(disposition)
  <> " after "
  <> int.to_string(fix_cycles)
  <> " fix cycle(s); "
  <> ledger_note
  <> ". "
  <> detail
}

// --- helpers ---------------------------------------------------------------------

/// The child's own workflow id — the per-brief scope the workspace path and
/// the Norn session ids are keyed on.
fn engine_id() -> Result(String, RemediationError) {
  case workflow.id() {
    Ok(id) -> Ok(id)
    Error(engine_error) ->
      Error(StageFailed(
        stage: "workflow_id",
        message: "could not read the child workflow id: "
          <> string.inspect(engine_error),
      ))
  }
}

fn stage_error(
  stage: String,
  activity_error: error.ActivityError,
) -> Result(value, RemediationError) {
  Error(StageFailed(stage: stage, message: activity_message(activity_error)))
}

fn activity_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ActivityEngineFailure(message: message) -> message
  }
}

/// `use`-friendly bind over `Result` with [`RemediationError`].
fn try(
  result: Result(a, RemediationError),
  next: fn(a) -> Result(b, RemediationError),
) -> Result(b, RemediationError) {
  case result {
    Ok(value) -> next(value)
    Error(remediation_error) -> Error(remediation_error)
  }
}
