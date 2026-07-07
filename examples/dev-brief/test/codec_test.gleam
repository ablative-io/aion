//// Codec contract tests: workflow input decoding (including the overridable
//// defaults), agent-output mirrors, and encode/decode roundtrips for the
//// wire types the Rust worker and the child-spawn boundary depend on.

import dev_brief/codecs
import dev_brief/types.{
  Accept, AcceptanceClaim, Blocking, Brief, BriefInput, BriefResult,
  CycleCapExhausted, DevReport, Deviation, GateCommand, GateCommandRun,
  GateOutcome, Lens, LensInput, LensVerdict, Reject, ReviewFinding, RunConfig,
}
import gleam/option.{None, Some}
import gleeunit/should

fn sample_brief() -> types.Brief {
  Brief(
    id: "DB-1",
    title: "Fix the frobnicator",
    objective: "Make it frob",
    context: "It has never frobbed",
    pointers: ["src/frob.rs"],
    scope_in: ["src/frob.rs"],
    scope_out: ["do not reorganize src/big_file.rs"],
    acceptance: ["frob() returns Ok on empty input"],
    notes: "",
  )
}

fn sample_report() -> types.DevReport {
  DevReport(
    brief_id: "DB-1",
    summary: "frobbed",
    commits: ["abc123"],
    acceptance_claims: [
      AcceptanceClaim(criterion: "frob() returns Ok on empty input", how: "x"),
    ],
    deviations: [Deviation(what: "renamed helper", why: "clarity")],
  )
}

pub fn brief_input_roundtrip_test() {
  let input =
    BriefInput(
      brief: sample_brief(),
      config: RunConfig(
        repo_root: "/repo",
        base_branch: "main",
        gates: [GateCommand(name: "fmt", argv: ["cargo", "fmt"])],
        max_fix_cycles: 2,
        lenses: [Lens(name: "correctness", charter: "hunt bugs")],
      ),
    )
  let codec = codecs.brief_input_codec()
  codec.encode(input)
  |> codec.decode
  |> should.equal(Ok(input))
}

pub fn brief_input_omitted_config_fields_resolve_to_defaults_test() {
  // A minimal operator-authored input: no base_branch, no max_fix_cycles,
  // no lenses, no gates — the decoder fills the documented defaults (the
  // lens default resolves later, in the workflow, so an explicit empty list
  // stays visible).
  let raw =
    "{\"brief\":{\"id\":\"DB-2\",\"title\":\"t\",\"objective\":\"o\","
    <> "\"acceptance\":[\"a\"]},\"config\":{\"repo_root\":\"/repo\"}}"
  case codecs.brief_input_codec().decode(raw) {
    Ok(input) -> {
      input.config.base_branch
      |> should.equal(types.default_base_branch())
      input.config.max_fix_cycles
      |> should.equal(types.default_max_fix_cycles())
      input.config.gates
      |> should.equal([])
      input.config.lenses
      |> should.equal([])
      input.brief.pointers
      |> should.equal([])
      input.brief.notes
      |> should.equal("")
    }
    Error(_) -> should.fail()
  }
}

pub fn dev_report_roundtrip_test() {
  let codec = codecs.dev_report_codec()
  codec.encode(sample_report())
  |> codec.decode
  |> should.equal(Ok(sample_report()))
}

pub fn lens_verdict_roundtrip_test() {
  let verdict =
    LensVerdict(
      lens: "correctness",
      findings: [
        ReviewFinding(severity: Blocking, title: "boom", evidence: "x:1"),
      ],
      overall: Reject,
      reject_reason: Some("boom"),
    )
  let codec = codecs.lens_verdict_codec()
  codec.encode(verdict)
  |> codec.decode
  |> should.equal(Ok(verdict))
}

pub fn lens_verdict_omitted_reject_reason_decodes_as_none_test() {
  // The agent schema marks reject_reason conditional; an accepting verdict
  // may omit the key entirely.
  let raw = "{\"lens\":\"regressions\",\"findings\":[],\"overall\":\"accept\"}"
  codecs.lens_verdict_codec().decode(raw)
  |> should.equal(
    Ok(LensVerdict(
      lens: "regressions",
      findings: [],
      overall: Accept,
      reject_reason: None,
    )),
  )
}

pub fn lens_input_roundtrip_test() {
  let input =
    LensInput(
      lens: Lens(name: "correctness", charter: "hunt"),
      brief: sample_brief(),
      diff: "diff --git a b",
      report: sample_report(),
      gate_runs: [
        GateCommandRun(
          name: "test",
          exit_code: 0,
          passed: True,
          output_tail: "ok",
        ),
      ],
    )
  let codec = codecs.lens_input_codec()
  codec.encode(input)
  |> codec.decode
  |> should.equal(Ok(input))
}

pub fn brief_result_roundtrip_test() {
  let result =
    BriefResult(
      brief_id: "DB-1",
      disposition: CycleCapExhausted,
      fix_cycles: 3,
      first_pass_accepted: False,
      verdict_mismatches: ["cycle 2: lens correctness: asserted overall ..."],
      branch: "dev/DB-1",
      report: Some(sample_report()),
      gate: Some(GateOutcome(
        pass: True,
        runs: [
          GateCommandRun(
            name: "clippy",
            exit_code: 0,
            passed: True,
            output_tail: "",
          ),
        ],
        diff: "diff",
        diagnostics: "",
      )),
      verdicts: [
        LensVerdict(
          lens: "correctness",
          findings: [],
          overall: Accept,
          reject_reason: None,
        ),
      ],
      workspace_removed: True,
      summary: "Brief DB-1: cycle_cap_exhausted after 3 fix cycle(s)",
    )
  let codec = codecs.brief_result_codec()
  codec.encode(result)
  |> codec.decode
  |> should.equal(Ok(result))
}

pub fn error_codec_roundtrip_test() {
  let codec = codecs.dev_brief_error_codec()
  codec.encode(types.StageFailed(stage: "run_gates", message: "boom"))
  |> codec.decode
  |> should.equal(Ok(types.StageFailed(stage: "run_gates", message: "boom")))
  codec.encode(types.ChildFailed(reason: "lost"))
  |> codec.decode
  |> should.equal(Ok(types.ChildFailed(reason: "lost")))
}
