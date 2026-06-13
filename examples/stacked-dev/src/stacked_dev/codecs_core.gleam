//// JSON codecs for the workspace, startup, dev, and scoped-check types.
////
//// Workflow-level and review/land codecs live in `stacked_dev/codecs_flow`.

import aion/codec
import aion_stacked_dev_io as stage_io
import gleam/dynamic/decode
import gleam/json
import stacked_dev/types.{
  type BuildWarm, type CheckResult, type CheckVerdict, type DevInput,
  type DevResult, type Isolation, type Placement, type ProvisionInput,
  type ResumeInput, type ScopedInput, type ScoutInput, type StartupResult,
  type StartupTask, type Workspace, BuildWarm, CheckFail, CheckPass, CheckResult,
  Copy, DevInput, DevResult, DevTask, Developed, Local, Overlay, ProvisionInput,
  Remote, ResumeInput, ScopedInput, ScoutInput, Vm, WarmTask, Warmed, Workspace,
  Worktree,
}

/// Wire name for a `Placement` value.
pub fn placement_to_string(placement: Placement) -> String {
  case placement {
    Local -> "local"
    Remote -> "remote"
  }
}

/// Wire name for an `Isolation` value.
pub fn isolation_to_string(isolation: Isolation) -> String {
  case isolation {
    Worktree -> "worktree"
    Copy -> "copy"
    Overlay -> "overlay"
    Vm -> "vm"
  }
}

fn placement_decoder() -> decode.Decoder(Placement) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "local" -> decode.success(Local)
      "remote" -> decode.success(Remote)
      _ -> decode.failure(Local, "local or remote")
    }
  })
}

fn isolation_decoder() -> decode.Decoder(Isolation) {
  decode.then(decode.string, fn(raw) {
    case raw {
      "worktree" -> decode.success(Worktree)
      "copy" -> decode.success(Copy)
      "overlay" -> decode.success(Overlay)
      "vm" -> decode.success(Vm)
      _ -> decode.failure(Worktree, "worktree, copy, overlay, or vm")
    }
  })
}

/// Codec for the `provision_workspace` activity input.
pub fn provision_input_codec() -> codec.Codec(ProvisionInput) {
  codec.json_codec(
    fn(input: ProvisionInput) {
      json.object([
        #("repo_root", json.string(input.repo_root)),
        #("brief_id", json.string(input.brief_id)),
        #("base_ref", json.string(input.base_ref)),
        #("placement", json.string(placement_to_string(input.placement))),
        #("isolation", json.string(isolation_to_string(input.isolation))),
      ])
    },
    provision_input_decoder(),
  )
}

/// Decoder for the provisioning fields. Shared with the `stacked_dev` input
/// codec, whose top-level object carries the same four fields.
pub fn provision_input_decoder() -> decode.Decoder(ProvisionInput) {
  use repo_root <- decode.field("repo_root", decode.string)
  use brief_id <- decode.field("brief_id", decode.string)
  use base_ref <- decode.field("base_ref", decode.string)
  use placement <- decode.field("placement", placement_decoder())
  use isolation <- decode.field("isolation", isolation_decoder())
  decode.success(ProvisionInput(
    repo_root: repo_root,
    brief_id: brief_id,
    base_ref: base_ref,
    placement: placement,
    isolation: isolation,
  ))
}

/// JSON encoder for a `Workspace`, shared with the input codecs that embed
/// workspaces.
pub fn workspace_to_json(workspace: Workspace) -> json.Json {
  json.object([
    #("path", json.string(workspace.path)),
    #("branch", json.string(workspace.branch)),
    #("placement", json.string(placement_to_string(workspace.placement))),
    #("isolation", json.string(isolation_to_string(workspace.isolation))),
  ])
}

/// Decoder for a `Workspace`, shared with the input codecs that embed
/// workspaces.
pub fn workspace_decoder() -> decode.Decoder(Workspace) {
  use path <- decode.field("path", decode.string)
  use branch <- decode.field("branch", decode.string)
  use placement <- decode.field("placement", placement_decoder())
  use isolation <- decode.field("isolation", isolation_decoder())
  decode.success(Workspace(
    path: path,
    branch: branch,
    placement: placement,
    isolation: isolation,
  ))
}

/// Codec for the `provision_workspace` activity output.
pub fn workspace_codec() -> codec.Codec(Workspace) {
  codec.json_codec(workspace_to_json, workspace_decoder())
}

/// JSON encoder for the advisory warm-build outcome.
pub fn build_warm_to_json(build_warm: BuildWarm) -> json.Json {
  json.object([
    #("ok", json.bool(build_warm.ok)),
    #("duration_ms", json.int(build_warm.duration_ms)),
  ])
}

/// Decoder for the advisory warm-build outcome.
pub fn build_warm_decoder() -> decode.Decoder(BuildWarm) {
  use ok <- decode.field("ok", decode.bool)
  use duration_ms <- decode.field("duration_ms", decode.int)
  decode.success(BuildWarm(ok: ok, duration_ms: duration_ms))
}

/// JSON encoder for a `DevInput` — the workspace and the projected prompt
/// (BD-003: the four document strings are gone; the prompt is built in
/// workflow code from BD-002's projections).
pub fn dev_input_to_json(input: DevInput) -> json.Json {
  json.object([
    #("workspace", workspace_to_json(input.workspace)),
    #("prompt", json.string(input.prompt)),
  ])
}

/// Decoder for a `DevInput`.
pub fn dev_input_decoder() -> decode.Decoder(DevInput) {
  use workspace <- decode.field("workspace", workspace_decoder())
  use prompt <- decode.field("prompt", decode.string)
  decode.success(DevInput(workspace: workspace, prompt: prompt))
}

/// Codec for the `scout` activity input: the workspace and the projected
/// scout prompt (BD-003).
pub fn scout_input_codec() -> codec.Codec(ScoutInput) {
  codec.json_codec(
    fn(input: ScoutInput) {
      json.object([
        #("workspace", workspace_to_json(input.workspace)),
        #("prompt", json.string(input.prompt)),
      ])
    },
    {
      use workspace <- decode.field("workspace", workspace_decoder())
      use prompt <- decode.field("prompt", decode.string)
      decode.success(ScoutInput(workspace: workspace, prompt: prompt))
    },
  )
}

/// JSON encoder for a `DevResult`, shared by the dev/resume codecs and the
/// CLI stdout parser in `stacked_dev/locals`.
pub fn dev_result_to_json(result: DevResult) -> json.Json {
  json.object([
    #("session_id", json.string(result.session_id)),
    #("files_touched", json.array(result.files_touched, json.string)),
    #("summary", json.string(result.summary)),
  ])
}

/// Decoder for a `DevResult`.
pub fn dev_result_decoder() -> decode.Decoder(DevResult) {
  use session_id <- decode.field("session_id", decode.string)
  use files_touched <- decode.field("files_touched", decode.list(decode.string))
  use summary <- decode.field("summary", decode.string)
  decode.success(DevResult(
    session_id: session_id,
    files_touched: files_touched,
    summary: summary,
  ))
}

/// Codec for the `dev_resume` activity output.
pub fn dev_result_codec() -> codec.Codec(DevResult) {
  codec.json_codec(dev_result_to_json, dev_result_decoder())
}

/// Real norn's `--output-format json` completion envelope decoder, generic
/// over the stage report's inner decoder: the schema-constrained result sits
/// under `"output"`, alongside usage/model/event fields this workflow ignores
/// (confirmed live, 2026-06-13). This is the codec seam for C31's parsing
/// rule — the locals accept the bare report shape OR this `{"output": ...}`
/// envelope and nothing else.
pub fn report_envelope_decoder(
  inner: decode.Decoder(report),
) -> decode.Decoder(report) {
  use report <- decode.field("output", inner)
  decode.success(report)
}

/// Decoder for real norn's dev-report completion envelope (the `{"output":
/// <dev report>}` shape), used by the dev/dev_resume locals.
pub fn dev_report_envelope_decoder() -> decode.Decoder(stage_io.DevReport) {
  report_envelope_decoder(stage_io.dev_report_decoder())
}

/// Codec for the startup fan-out input envelope shared by the `warm_build`
/// and `dev` activities (see `types.StartupTask`).
pub fn startup_task_codec() -> codec.Codec(StartupTask) {
  codec.json_codec(
    fn(task: StartupTask) {
      case task {
        WarmTask(workspace: workspace) ->
          json.object([
            #("task", json.string("warm_build")),
            #("workspace", workspace_to_json(workspace)),
          ])
        DevTask(dev_input: dev_input) ->
          json.object([
            #("task", json.string("dev")),
            #("dev_input", dev_input_to_json(dev_input)),
          ])
      }
    },
    {
      use task <- decode.field("task", decode.string)
      case task {
        "warm_build" -> {
          use workspace <- decode.field("workspace", workspace_decoder())
          decode.success(WarmTask(workspace: workspace))
        }
        "dev" -> {
          use dev_input <- decode.field("dev_input", dev_input_decoder())
          decode.success(DevTask(dev_input: dev_input))
        }
        _ ->
          decode.failure(
            WarmTask(workspace: fallback_workspace()),
            "warm_build or dev",
          )
      }
    },
  )
}

/// Codec for the startup fan-out output envelope shared by the `warm_build`
/// and `dev` activities (see `types.StartupResult`).
pub fn startup_result_codec() -> codec.Codec(StartupResult) {
  codec.json_codec(
    fn(result: StartupResult) {
      case result {
        Warmed(build_warm: build_warm) ->
          json.object([
            #("task", json.string("warm_build")),
            #("build_warm", build_warm_to_json(build_warm)),
          ])
        Developed(dev_report: dev_report) ->
          json.object([
            #("task", json.string("dev")),
            #("dev_report", stage_io.dev_report_to_json(dev_report)),
          ])
      }
    },
    {
      use task <- decode.field("task", decode.string)
      case task {
        "warm_build" -> {
          use build_warm <- decode.field("build_warm", build_warm_decoder())
          decode.success(Warmed(build_warm: build_warm))
        }
        "dev" -> {
          use dev_report <- decode.field(
            "dev_report",
            stage_io.dev_report_decoder(),
          )
          decode.success(Developed(dev_report: dev_report))
        }
        _ ->
          decode.failure(
            Warmed(build_warm: BuildWarm(ok: False, duration_ms: 0)),
            "warm_build or dev",
          )
      }
    },
  )
}

/// Codec for the `scoped_checks` activity input.
pub fn scoped_input_codec() -> codec.Codec(ScopedInput) {
  codec.json_codec(
    fn(input: ScopedInput) {
      json.object([
        #("workspace", workspace_to_json(input.workspace)),
        #("files_touched", json.array(input.files_touched, json.string)),
      ])
    },
    {
      use workspace <- decode.field("workspace", workspace_decoder())
      use files_touched <- decode.field(
        "files_touched",
        decode.list(decode.string),
      )
      decode.success(ScopedInput(
        workspace: workspace,
        files_touched: files_touched,
      ))
    },
  )
}

/// Codec for the `scoped_checks` activity output.
pub fn check_result_codec() -> codec.Codec(CheckResult) {
  codec.json_codec(
    fn(result: CheckResult) {
      json.object([
        #("verdict", check_verdict_to_json(result.verdict)),
        #("affected_modules", json.array(result.affected_modules, json.string)),
        #("checked_scope", json.string(result.checked_scope)),
      ])
    },
    {
      use verdict <- decode.field("verdict", check_verdict_decoder())
      use affected_modules <- decode.field(
        "affected_modules",
        decode.list(decode.string),
      )
      use checked_scope <- decode.field("checked_scope", decode.string)
      decode.success(CheckResult(
        verdict: verdict,
        affected_modules: affected_modules,
        checked_scope: checked_scope,
      ))
    },
  )
}

fn check_verdict_to_json(verdict: CheckVerdict) -> json.Json {
  case verdict {
    CheckPass -> json.object([#("outcome", json.string("pass"))])
    CheckFail(diagnostics: diagnostics) ->
      json.object([
        #("outcome", json.string("fail")),
        #("diagnostics", json.string(diagnostics)),
      ])
  }
}

fn check_verdict_decoder() -> decode.Decoder(CheckVerdict) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "pass" -> decode.success(CheckPass)
    "fail" -> {
      use diagnostics <- decode.field("diagnostics", decode.string)
      decode.success(CheckFail(diagnostics: diagnostics))
    }
    _ -> decode.failure(CheckPass, "pass or fail")
  }
}

/// Codec for the `dev_resume` activity input.
pub fn resume_input_codec() -> codec.Codec(ResumeInput) {
  codec.json_codec(
    fn(input: ResumeInput) {
      json.object([
        #("session_id", json.string(input.session_id)),
        #("feedback", json.string(input.feedback)),
      ])
    },
    {
      use session_id <- decode.field("session_id", decode.string)
      use feedback <- decode.field("feedback", decode.string)
      decode.success(ResumeInput(session_id: session_id, feedback: feedback))
    },
  )
}

/// Zero value used only inside decoder failure branches, where
/// `gleam/dynamic/decode` requires a representative value of the decoded
/// type. It never escapes a successful decode.
fn fallback_workspace() -> Workspace {
  Workspace(path: "", branch: "", placement: Local, isolation: Worktree)
}
