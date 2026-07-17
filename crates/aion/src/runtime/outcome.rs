//! Workflow process outcome conversion at the beamr boundary.

use aion_core::{Payload, WorkflowError};
use beamr::atom::{Atom, AtomTable};
use beamr::process::ExitReason;
use beamr::term::Term;
use beamr::term::boxed::Tuple;

use crate::{EngineError, Pid};

use super::payload::term_to_payload;
use super::process_exit::OwnedProcessExitOutcome;

/// Runtime-converted terminal workflow process outcome.
#[derive(Clone)]
pub enum WorkflowProcessOutcome {
    /// The workflow process returned normally with a payload result.
    Completed(Payload),
    /// The workflow process exited abnormally with a workflow error.
    Failed(WorkflowError),
}

pub(super) fn workflow_outcome(
    atoms: &AtomTable,
    pid: Pid,
    owned: &OwnedProcessExitOutcome,
) -> Result<Result<Payload, WorkflowError>, EngineError> {
    match workflow_process_outcome(atoms, pid, owned)? {
        WorkflowProcessOutcome::Completed(payload) => Ok(Ok(payload)),
        WorkflowProcessOutcome::Failed(error) => Ok(Err(error)),
    }
}

pub(super) fn workflow_process_outcome(
    atoms: &AtomTable,
    pid: Pid,
    owned: &OwnedProcessExitOutcome,
) -> Result<WorkflowProcessOutcome, EngineError> {
    let observed = match owned {
        OwnedProcessExitOutcome::Observed(observed) => observed,
        OwnedProcessExitOutcome::DeadAndUnavailable { process_id } => {
            return Err(EngineError::ProcessExitUnavailable {
                process_id: *process_id,
            });
        }
    };
    if observed.reason != ExitReason::Normal {
        // The VM execution error, when present, is the authoritative exit
        // cause. A caught raise can leave a residual exception until try_end.
        if let Some(error) = &observed.execution_error {
            let formatted = error.format_with_atoms(atoms);
            let residue = observed
                .exception
                .as_ref()
                .map_or_else(String::new, |exception| {
                    format!(
                        " (residual exception: {})",
                        exception.format_with_atoms(atoms)
                    )
                });
            return Ok(WorkflowProcessOutcome::Failed(WorkflowError {
                message: format!(
                    "workflow process {pid} exited: {:?}: VM execution error: {formatted}{residue}",
                    observed.reason
                ),
                details: None,
            }));
        }
        if let Some(exception) = &observed.exception {
            let formatted = exception.format_with_atoms(atoms);
            let view = exception.view();
            let details = term_to_payload(view.reason, atoms).ok();
            return Ok(WorkflowProcessOutcome::Failed(WorkflowError {
                message: format!("workflow process {pid} exited: {formatted}"),
                details,
            }));
        }
    }
    convert_process_outcome(atoms, pid, observed.reason, observed.result.root())
}

pub(super) fn convert_process_outcome(
    atoms: &AtomTable,
    pid: Pid,
    reason: ExitReason,
    result: Term,
) -> Result<WorkflowProcessOutcome, EngineError> {
    if reason == ExitReason::Normal {
        unwrap_gleam_result(result, atoms, pid)
    } else {
        let formatted = beamr::term::format::format_term(result, atoms);
        let details = term_to_payload(result, atoms).ok();
        Ok(WorkflowProcessOutcome::Failed(WorkflowError {
            message: format!("workflow process {pid} exited: {reason:?}: {formatted}"),
            details,
        }))
    }
}

fn unwrap_gleam_result(
    result: Term,
    atoms: &AtomTable,
    pid: Pid,
) -> Result<WorkflowProcessOutcome, EngineError> {
    if let Some(tuple) = Tuple::new(result) {
        if tuple.arity() == 2 {
            if let (Some(tag), Some(value)) = (tuple.get(0), tuple.get(1)) {
                if let Some(atom) = tag.as_atom() {
                    if atom == Atom::OK {
                        return Ok(WorkflowProcessOutcome::Completed(term_to_payload(
                            value, atoms,
                        )?));
                    }
                    if atom == Atom::ERROR {
                        let details = term_to_payload(value, atoms).ok();
                        return Ok(WorkflowProcessOutcome::Failed(WorkflowError {
                            message: format!("workflow {pid} returned error"),
                            details,
                        }));
                    }
                }
            }
        }
    }
    Ok(WorkflowProcessOutcome::Completed(term_to_payload(
        result, atoms,
    )?))
}
