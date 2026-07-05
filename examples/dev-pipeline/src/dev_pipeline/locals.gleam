//// Local activity implementations — the `aion/testing` seam every
//// `activity.new` call binds.
////
//// Every dev-pipeline activity is served ONLY by the deployed norn worker
//// (`norn-worker/`), so these locals are loud typed terminals, never silent
//// stubs. When the hermetic test suite lands (stacked-dev pattern: fake-CLI
//// shims alone on PATH), these become real shelling implementations
//// mirroring the worker handler for handler.

import aion/error
import dev_pipeline/types.{
  type Brief, type GateCliRun, type GateRun, type ImplementRound,
  type ImplementationReport, type ProvisionInput, type Refutation,
  type ScoutReport, type TeardownInput, type TornDown, type Workspace,
}

/// Local `scout`: no local implementation in this slice.
pub fn scout(_prompt: String) -> Result(ScoutReport, error.ActivityError) {
  Error(no_local("scout"))
}

/// Local `design`: no local implementation in this slice.
pub fn design(_prompt: String) -> Result(Brief, error.ActivityError) {
  Error(no_local("design"))
}

/// Local `refute`: no local implementation in this slice.
pub fn refute(_prompt: String) -> Result(Refutation, error.ActivityError) {
  Error(no_local("refute"))
}

/// Local `provision_workspace`: no local implementation in this slice.
pub fn provision_workspace(
  _input: ProvisionInput,
) -> Result(Workspace, error.ActivityError) {
  Error(no_local("provision_workspace"))
}

/// Local `implement`: no local implementation in this slice.
pub fn implementer(
  _round: ImplementRound,
) -> Result(ImplementationReport, error.ActivityError) {
  Error(no_local("implement"))
}

/// Local `run_gate`: no local implementation in this slice.
pub fn run_gate(_gate_run: GateRun) -> Result(GateCliRun, error.ActivityError) {
  Error(no_local("run_gate"))
}

/// Local `implement_resume`: no local implementation in this slice.
pub fn implement_resume(
  _round: ImplementRound,
) -> Result(ImplementationReport, error.ActivityError) {
  Error(no_local("implement_resume"))
}

/// Local `teardown_workspace`: no local implementation in this slice.
pub fn teardown_workspace(
  _input: TeardownInput,
) -> Result(TornDown, error.ActivityError) {
  Error(no_local("teardown_workspace"))
}

fn no_local(name: String) -> error.ActivityError {
  error.terminal(
    name
    <> " has no local implementation in this slice — deploy the dev-pipeline "
    <> "norn worker (examples/dev-pipeline/norn-worker) to serve it",
  )
}
