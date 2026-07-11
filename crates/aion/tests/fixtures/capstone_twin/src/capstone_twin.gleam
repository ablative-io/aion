//// AWL BC-1 capstone twin (D-BC5).
////
//// The Gleam-built reference for the hand-constructed bytecode module in
//// `awl_bc1_capstone.rs`: the smallest workflow that calls into `aion_flow`
//// and leaves a durable trail (WorkflowStarted → TimerStarted → TimerFired →
//// WorkflowCompleted). The capstone hand-builds a bytecode module with the
//// same observable behaviour via the beamr `encode` API and asserts the two
//// normalized event trails are identical.

import aion/duration
import aion/workflow
import gleam/dynamic.{type Dynamic}

pub fn run(_raw_input: Dynamic) -> Result(String, String) {
  case workflow.sleep(duration.milliseconds(25)) {
    Ok(_) -> Ok("\"capstone\"")
    Error(_) -> Error("timer failed")
  }
}
