//// Wires the stage-contract schema drift gate (CN7) into the package test
//// suite: the gleeunit run fails whenever a package copy under `schemas/`
//// diverges byte-for-byte from the design-system canon.
////
//// `gleam test` runs from the package root, so the script is invoked through
//// `stacked_dev/cli.run` with the package root as the working directory; the
//// script itself resolves both schema directories from its own location.
//// bash is named by absolute path because the pipeline suite points the
//// VM-global `PATH` at exclusive shim directories (CN9) — the same
//// `/bin`-holds-the-real-tools assumption the shim scripts themselves
//// encode.

import gleeunit/should
import stacked_dev/cli

pub fn schema_drift_gate_passes_on_clean_tree_test() {
  case cli.run("/bin/bash", ["scripts/check-schema-drift.sh"], ".") {
    Ok(run) ->
      case cli.succeeded(run) {
        True -> Nil
        False -> {
          // Surface the script's own drift report in the failure output.
          should.equal(cli.run_diagnostics(run), "schema drift gate passed")
        }
      }
    Error(failure) ->
      // `bash` missing or unspawnable is a loud failure, never a silent skip.
      should.equal(cli.failure_message(failure), "schema drift gate ran")
  }
}
