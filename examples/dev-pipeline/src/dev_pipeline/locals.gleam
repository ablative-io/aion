//// Local activity implementations — the `aion/testing` seam every
//// `activity.new` call binds.
////
//// Slice-1 scope: brief-forge's three agent rounds are served ONLY by the
//// deployed norn worker (`norn-worker/`), so these locals are loud typed
//// terminals, never silent stubs. When the hermetic test suite lands
//// (stacked-dev pattern: fake-CLI shims alone on PATH), these become real
//// `norn`-shelling implementations mirroring the worker handler for handler.

import aion/error
import dev_pipeline/types.{
  type AgentRound, type Brief, type Refutation, type ScoutReport,
}

/// Local `scout`: no local implementation in this slice.
pub fn scout(_round: AgentRound) -> Result(ScoutReport, error.ActivityError) {
  Error(no_local("scout"))
}

/// Local `design`: no local implementation in this slice.
pub fn design(_round: AgentRound) -> Result(Brief, error.ActivityError) {
  Error(no_local("design"))
}

/// Local `refute`: no local implementation in this slice.
pub fn refute(_round: AgentRound) -> Result(Refutation, error.ActivityError) {
  Error(no_local("refute"))
}

fn no_local(name: String) -> error.ActivityError {
  error.terminal(
    name
    <> " has no local implementation in this slice — deploy the dev-pipeline "
    <> "norn worker (examples/dev-pipeline/norn-worker) to serve it",
  )
}
