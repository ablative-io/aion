//! Tier-2 in-VM activity child spawn and outcome propagation.
//!
//! This module only spawns an activity process, links it to the workflow
//! process, and provides the plumbing that surfaces the child outcome back to
//! that workflow. The AD append path records Activity events. AT policy
//! machinery consumes surfaced activity errors to decide any future retry.

use aion_core::{ActivityError, Payload};

use crate::{EngineError, Pid, RuntimeHandle, RuntimeInput};

/// Dispatch an in-VM activity as a child process linked to `parent_pid`.
///
/// Dirty scheduling is resolved by [`RuntimeHandle::spawn_activity`], which
/// looks up the installed NIF registration metadata for the module/function and
/// entry arity. The caller does not provide or guess a dirty flag.
///
/// # Errors
///
/// Returns [`EngineError::Runtime`] when payload conversion fails, the parent is
/// not live, or the runtime cannot spawn the linked child.
pub fn dispatch_activity(
    runtime: &RuntimeHandle,
    parent_pid: Pid,
    deployed_module: &str,
    function: &str,
    input: &Payload,
) -> Result<Pid, EngineError> {
    runtime.spawn_activity(
        parent_pid,
        deployed_module,
        function,
        RuntimeInput::from_payload(input)?,
    )
}

/// Surface an already exited activity child's outcome to its workflow parent.
///
/// Normal completion stores a result [`Payload`] for the parent and queues a
/// runtime-owned result marker in the workflow mailbox. Abnormal completion
/// stores the typed [`ActivityError`] associated with the trapped exit signal;
/// dispatch preserves the error classification and makes no policy decision.
///
/// # Errors
///
/// Returns [`EngineError::Runtime`] when the parent is no longer live or the
/// runtime cannot translate/deliver the activity outcome.
pub fn propagate_activity_outcome(
    runtime: &RuntimeHandle,
    parent_pid: Pid,
    activity_pid: Pid,
) -> Result<(), EngineError> {
    runtime.propagate_activity_outcome(parent_pid, activity_pid)
}

/// Attach a typed activity error to a trapped activity EXIT signal.
///
/// This helper is used by the runtime/AT seam when an activity implementation
/// has produced a classified [`ActivityError`] before the linked process exits.
///
/// # Errors
///
/// Returns [`EngineError::Runtime`] when the workflow process is no longer live.
pub fn surface_activity_error(
    runtime: &RuntimeHandle,
    parent_pid: Pid,
    activity_pid: Pid,
    error: ActivityError,
) -> Result<(), EngineError> {
    runtime.deliver_activity_error(parent_pid, activity_pid, error)
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityErrorKind, ContentType};
    use serde_json::json;

    use super::{dispatch_activity, propagate_activity_outcome, surface_activity_error};
    use crate::runtime::RuntimeConfig;
    use crate::{EngineError, RuntimeHandle};

    fn runtime() -> Result<RuntimeHandle, EngineError> {
        RuntimeHandle::new(RuntimeConfig::new(Some(1)))
    }

    fn payload() -> Result<aion_core::Payload, aion_core::PayloadError> {
        aion_core::Payload::from_json(&json!(null))
    }

    fn fixture_workflow_beam() -> &'static [u8] {
        include_bytes!("../../tests/fixtures/aion_fixture_workflow.beam")
    }

    #[test]
    fn dispatch_spawns_linked_child_and_uses_dirty_registration(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let runtime = runtime()?;
        runtime.install_test_activity_nif("activity_host", "answer", true, true)?;
        runtime.register_native_call_module_for_test(
            "activity_mod",
            "run",
            "activity_host",
            "answer",
        );
        let workflow = runtime.spawn_test_process_with_trap_exit(true)?;

        let activity = dispatch_activity(&runtime, workflow, "activity_mod", "run", &payload()?)?;

        assert!(runtime.is_linked(workflow, activity)?);
        assert!(runtime.is_dirty("activity_host", "answer"));
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn successful_activity_result_is_delivered_to_workflow(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let runtime = runtime()?;
        runtime.install_test_activity_nif("activity_host", "answer", false, true)?;
        runtime.register_native_call_module_for_test(
            "activity_ok",
            "run",
            "activity_host",
            "answer",
        );
        let workflow = runtime.spawn_test_process_with_trap_exit(true)?;
        let activity = dispatch_activity(&runtime, workflow, "activity_ok", "run", &payload()?)?;

        propagate_activity_outcome(&runtime, workflow, activity)?;

        let result = runtime.activity_result(workflow, activity);
        assert_eq!(result, Some(aion_core::Payload::from_json(&json!(42))?));
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn failing_activity_surfaces_typed_error_with_trapped_exit(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let runtime = runtime()?;
        runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;
        let workflow = runtime.spawn_test_process_with_trap_exit(true)?;
        let activity = dispatch_activity(
            &runtime,
            workflow,
            "aion_fixture_workflow",
            "activity",
            &payload()?,
        )?;
        assert!(runtime.is_linked(workflow, activity)?);
        let details = aion_core::Payload::new(ContentType::Json, br#"{"code":"boom"}"#.to_vec());
        let error = aion_core::ActivityError {
            kind: ActivityErrorKind::Retryable,
            message: String::from("boom"),
            details: Some(details),
        };
        surface_activity_error(&runtime, workflow, activity, error.clone())?;
        runtime.terminate_test_process_with_error(activity)?;

        propagate_activity_outcome(&runtime, workflow, activity)?;

        assert!(runtime.has_trapped_exit_message(workflow, activity)?);
        assert_eq!(runtime.activity_error(workflow, activity), Some(error));
        runtime.shutdown()?;
        Ok(())
    }
}
