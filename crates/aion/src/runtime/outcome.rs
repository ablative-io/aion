//! Workflow process outcome conversion at the beamr boundary.

use aion_core::{Payload, WorkflowError};
use beamr::atom::AtomTable;
use beamr::process::ExitReason;
use beamr::scheduler::Scheduler;

use crate::{EngineError, Pid};

use super::payload::term_to_payload;

pub(super) fn workflow_outcome(
    scheduler: &Scheduler,
    atoms: &AtomTable,
    pid: Pid,
) -> Result<Result<Payload, WorkflowError>, EngineError> {
    let (reason, result) = scheduler.run_until_exit(pid);
    if reason == ExitReason::Normal {
        Ok(Ok(term_to_payload(result, atoms)?))
    } else {
        Ok(Err(WorkflowError {
            message: format!("workflow process {pid} exited: {reason:?}"),
            details: None,
        }))
    }
}
