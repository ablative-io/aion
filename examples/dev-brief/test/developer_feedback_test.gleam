//// The developer loop-back gate-feedback projection
//// (`dev_brief.developer_gate_feedback`): the developer's resumed session
//// needs the FAILING commands' evidence and nothing else — passing runs keep
//// only their verdict line, `diff` and `diagnostics` are stripped. The
//// lenses' path is untouched (they receive the full outcome), so these tests
//// pin the projection itself.

import dev_brief
import dev_brief/types.{GateCommandRun, GateOutcome}
import gleeunit/should

fn sample_outcome() -> types.GateOutcome {
  GateOutcome(
    pass: False,
    runs: [
      GateCommandRun(
        name: "fmt",
        argv: ["cargo", "fmt"],
        exit_code: 0,
        passed: True,
        output_tail: "formatted 3 files",
      ),
      GateCommandRun(
        name: "clippy",
        argv: ["cargo", "clippy"],
        exit_code: 101,
        passed: False,
        output_tail: "warning: used `expect`",
      ),
    ],
    diff: "diff --git a/src/lib.rs b/src/lib.rs",
    diagnostics: "gate `clippy` exited 101:\nwarning: used `expect`",
  )
}

pub fn passing_runs_keep_only_their_verdict_line_test() {
  let feedback = dev_brief.developer_gate_feedback(sample_outcome())
  feedback.runs
  |> should.equal([
    GateCommandRun(
      name: "fmt",
      argv: ["cargo", "fmt"],
      exit_code: 0,
      passed: True,
      output_tail: "",
    ),
    GateCommandRun(
      name: "clippy",
      argv: ["cargo", "clippy"],
      exit_code: 101,
      passed: False,
      output_tail: "warning: used `expect`",
    ),
  ])
}

pub fn diff_and_diagnostics_are_stripped_test() {
  let feedback = dev_brief.developer_gate_feedback(sample_outcome())
  feedback.diff |> should.equal("")
  feedback.diagnostics |> should.equal("")
}

pub fn the_overall_verdict_is_preserved_test() {
  let feedback = dev_brief.developer_gate_feedback(sample_outcome())
  feedback.pass |> should.equal(False)

  let green =
    dev_brief.developer_gate_feedback(GateOutcome(
      pass: True,
      runs: [
        GateCommandRun(
          name: "clippy",
          argv: ["cargo", "clippy"],
          exit_code: 0,
          passed: True,
          output_tail: "clean",
        ),
      ],
      diff: "",
      diagnostics: "all 1 gate command(s) green",
    ))
  green.pass |> should.equal(True)
  green.runs
  |> should.equal([
    GateCommandRun(
      name: "clippy",
      argv: ["cargo", "clippy"],
      exit_code: 0,
      passed: True,
      output_tail: "",
    ),
  ])
}
