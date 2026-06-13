//// Typed activity values for the stacked-dev workflow family.
////
//// Every activity name built here must be declared in the `activities` list
//// of the `workflow.toml` entry that dispatches it. The final argument to
//// each `activity.new` is the local implementation from
//// `stacked_dev/locals` — the test seam that shells to the real CLI under
//// the `aion/testing` harness. Deployed, a Meridian worker registers the
//// same names.

import aion/activity
import stacked_dev/codecs_brief
import stacked_dev/codecs_core
import stacked_dev/codecs_flow
import stacked_dev/locals
import stacked_dev/types.{
  type DevInput, type EnrichInput, type GateInput, type LandInput,
  type ProvisionInput, type ResumeInput, type ReviewRequest, type ScopedInput,
  DevTask, WarmTask,
}

/// Activity name served by the provisioning worker.
pub const provision_workspace_name = "provision_workspace"

/// Activity name served by the warm-build worker.
pub const warm_build_name = "warm_build"

/// Activity name served by the dev (norn run) worker.
pub const dev_name = "dev"

/// Activity name served by the scoped-verification worker.
pub const scoped_checks_name = "scoped_checks"

/// Activity name served by the dev-resume (norn resume) worker.
pub const dev_resume_name = "dev_resume"

/// Activity name served by the authoritative-gate worker.
pub const full_checks_name = "full_checks"

/// Activity name served by the review-request worker.
pub const request_review_name = "request_review"

/// Activity name served by the landing (`yg branch merge`) worker.
pub const land_name = "land"

/// Activity name served by the brief-enrichment worker. One name serves all
/// four write points (after scout, after dev convergence, after review, and
/// the execution block before land) — the `Enrichment` variant selects the
/// merge.
pub const enrich_brief_name = "enrich_brief"

/// `provision_workspace`: provision an isolated workspace off the base ref.
pub fn provision_workspace(
  input: ProvisionInput,
) -> activity.Activity(ProvisionInput, types.Workspace) {
  activity.new(
    provision_workspace_name,
    input,
    codecs_core.provision_input_codec(),
    codecs_core.workspace_codec(),
    locals.provision_workspace,
  )
}

/// `warm_build`: advisory cache warming, dispatched concurrently with `dev`
/// through `workflow.all`, hence the shared startup envelope codecs.
pub fn warm_build(
  workspace: types.Workspace,
) -> activity.Activity(types.StartupTask, types.StartupResult) {
  activity.new(
    warm_build_name,
    WarmTask(workspace: workspace),
    codecs_core.startup_task_codec(),
    codecs_core.startup_result_codec(),
    locals.startup_task,
  )
}

/// `dev`: the dev agent round, dispatched concurrently with `warm_build`
/// through `workflow.all`, hence the shared startup envelope codecs.
pub fn dev(
  dev_input: DevInput,
) -> activity.Activity(types.StartupTask, types.StartupResult) {
  activity.new(
    dev_name,
    DevTask(dev_input: dev_input),
    codecs_core.startup_task_codec(),
    codecs_core.startup_result_codec(),
    locals.startup_task,
  )
}

/// `scoped_checks`: the fast inner verification limited to affected modules.
pub fn scoped_checks(
  input: ScopedInput,
) -> activity.Activity(ScopedInput, types.CheckResult) {
  activity.new(
    scoped_checks_name,
    input,
    codecs_core.scoped_input_codec(),
    codecs_core.check_result_codec(),
    locals.scoped_checks,
  )
}

/// `dev_resume`: resume the same agent session with diagnostics or review
/// notes.
pub fn dev_resume(
  input: ResumeInput,
) -> activity.Activity(ResumeInput, types.DevResult) {
  activity.new(
    dev_resume_name,
    input,
    codecs_core.resume_input_codec(),
    codecs_core.dev_result_codec(),
    locals.dev_resume,
  )
}

/// `full_checks`: the authoritative gate body.
pub fn full_checks(
  input: GateInput,
) -> activity.Activity(GateInput, types.GateResult) {
  activity.new(
    full_checks_name,
    input,
    codecs_flow.gate_input_codec(),
    codecs_flow.gate_result_codec(),
    locals.full_checks,
  )
}

/// `request_review`: emit the review request; the verdict arrives by signal.
pub fn request_review(
  input: ReviewRequest,
) -> activity.Activity(ReviewRequest, types.ReviewAck) {
  activity.new(
    request_review_name,
    input,
    codecs_flow.review_request_codec(),
    codecs_flow.review_ack_codec(),
    locals.request_review,
  )
}

/// `land`: `yg branch merge` into the tree parent.
pub fn land(input: LandInput) -> activity.Activity(LandInput, types.Landed) {
  activity.new(
    land_name,
    input,
    codecs_flow.land_input_codec(),
    codecs_flow.landed_codec(),
    locals.land,
  )
}

/// `enrich_brief`: append one stage report or the execution block into the
/// brief file inside the run's worktree (ADR-007, ADR-009). The output codec
/// is BD-001's brief document codec — the same single codec function the
/// workflow input path uses.
pub fn enrich_brief(
  input: EnrichInput,
) -> activity.Activity(EnrichInput, types.BriefDocument) {
  activity.new(
    enrich_brief_name,
    input,
    codecs_flow.enrich_input_codec(),
    codecs_brief.brief_document_codec(),
    locals.enrich_brief,
  )
}
