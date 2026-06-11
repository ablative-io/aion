//! Engine-error-to-wire-error mapping for the shared handlers.

use aion_core::{WorkflowId, WorkflowStatus};
use aion_proto::WireError;

use crate::ServerError;

pub(super) fn map_start_error(error: aion::EngineError, workflow_type: &str) -> WireError {
    match error {
        aion::EngineError::WorkflowNotFound { .. } => WireError::not_found_with_type(
            "WorkflowTypeNotFound",
            format!("workflow type {workflow_type} is not registered"),
        ),
        other => ServerError::from(other).to_wire_error(),
    }
}

pub(super) fn map_workflow_operation_error(
    error: aion::EngineError,
    workflow_id: &WorkflowId,
) -> WireError {
    match error {
        aion::EngineError::WorkflowNotFound { .. } => workflow_not_found_error(workflow_id),
        other => ServerError::from(other).to_wire_error(),
    }
}

pub(super) fn workflow_not_found_error(workflow_id: &WorkflowId) -> WireError {
    WireError::not_found_with_type(
        "WorkflowNotFound",
        format!("workflow {workflow_id} not found"),
    )
}

pub(super) fn signal_terminal_error(workflow_id: &WorkflowId, status: WorkflowStatus) -> WireError {
    WireError::not_running_with_type(
        "WorkflowTerminal",
        format!("workflow {workflow_id} has already reached terminal state {status:?}"),
    )
}

pub(super) fn cancel_terminal_error(workflow_id: &WorkflowId, status: WorkflowStatus) -> WireError {
    WireError::not_running_with_type(
        "WorkflowTerminal",
        format!("workflow {workflow_id} has already completed with status {status:?}"),
    )
}

pub(super) fn log_server_error(
    operation: &'static str,
    namespace: Option<&str>,
    workflow_id: Option<&WorkflowId>,
    error: &ServerError,
) -> WireError {
    let fields = error.trace_fields();
    tracing::error!(
        operation,
        namespace,
        workflow_id = workflow_id.map(ToString::to_string).as_deref(),
        error_type = %fields.error_type,
        store_error_type = fields.store_error_type,
        reason = %fields.reason,
        "request handler failed"
    );
    error.to_wire_error()
}
