//! Workflow process outcome conversion at the beamr boundary.

use aion_core::{Payload, WorkflowError};
use beamr::atom::AtomTable;
use beamr::process::ExitReason;
use beamr::scheduler::Scheduler;
use beamr::term::Term;

use crate::{EngineError, Pid};

use super::payload::term_to_payload;

/// Runtime-converted terminal workflow process outcome.
pub enum WorkflowProcessOutcome {
    /// The workflow process returned normally with a payload result.
    Completed(Payload),
    /// The workflow process exited abnormally with a workflow error.
    Failed(WorkflowError),
}

pub(super) fn workflow_outcome(
    scheduler: &Scheduler,
    atoms: &AtomTable,
    pid: Pid,
) -> Result<Result<Payload, WorkflowError>, EngineError> {
    match workflow_process_outcome(scheduler, atoms, pid)? {
        WorkflowProcessOutcome::Completed(payload) => Ok(Ok(payload)),
        WorkflowProcessOutcome::Failed(error) => Ok(Err(error)),
    }
}

pub(super) fn workflow_process_outcome(
    scheduler: &Scheduler,
    atoms: &AtomTable,
    pid: Pid,
) -> Result<WorkflowProcessOutcome, EngineError> {
    let (reason, result) = scheduler.run_until_exit(pid);
    convert_process_outcome(atoms, pid, reason, result)
}

pub(super) fn convert_process_outcome(
    atoms: &AtomTable,
    pid: Pid,
    reason: ExitReason,
    result: Term,
) -> Result<WorkflowProcessOutcome, EngineError> {
    if reason == ExitReason::Normal {
        Ok(WorkflowProcessOutcome::Completed(term_to_payload(
            result, atoms,
        )?))
    } else {
        let details = term_to_payload(result, atoms).ok();
        Ok(WorkflowProcessOutcome::Failed(WorkflowError {
            message: format!("workflow process {pid} exited: {reason:?}"),
            details,
        }))
    }
}
