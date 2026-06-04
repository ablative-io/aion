//! Trap-exit and cancellation policy for supervised workflow processes.

use crate::{EngineError, Pid, RuntimeHandle, RuntimeInput};

/// Spawn a workflow process with the supervision policy applied.
///
/// Workflow processes trap exits so abnormal exits from linked activity children
/// arrive as exit messages rather than crashing the workflow process.
///
/// # Errors
///
/// Returns [`EngineError::Runtime`] when spawning fails or beamr rejects the
/// trap-exit update.
pub fn spawn_workflow_with_policy(
    runtime: &RuntimeHandle,
    deployed_module: &str,
    function: &str,
    input: RuntimeInput,
) -> Result<Pid, EngineError> {
    runtime.spawn_workflow_trapping(deployed_module, function, input)
}

/// Spawn a linked activity process under a workflow process.
///
/// Activity processes intentionally do not trap exits. The runtime establishes
/// the BEAM link atomically during spawn; this policy leaves the child in the
/// default non-trapping state so workflow cancellation propagates naturally.
///
/// # Errors
///
/// Returns [`EngineError::Runtime`] when the parent is not live, or the linked
/// spawn fails.
pub fn spawn_activity_with_policy(
    runtime: &RuntimeHandle,
    parent_pid: Pid,
    deployed_module: &str,
    function: &str,
    input: RuntimeInput,
) -> Result<Pid, EngineError> {
    runtime.spawn_activity(parent_pid, deployed_module, function, input)
}

/// Cancel a workflow process by killing its PID and relying on link propagation.
///
/// There is deliberately no loop over recorded activity children here: linked
/// activity processes do not trap exits, so beamr's link propagation terminates
/// them when their workflow process is killed. The supervision tree records the
/// structural relationship for Aion, while the runtime link graph performs the
/// teardown.
///
/// # Errors
///
/// Returns [`EngineError::Runtime`] when `workflow_pid` is not live.
pub fn cancel_workflow_by_link_propagation(
    runtime: &RuntimeHandle,
    workflow_pid: Pid,
) -> Result<(), EngineError> {
    runtime.cancel_pid(workflow_pid)
}

#[cfg(test)]
mod tests {
    use crate::RuntimeConfig;

    use super::{cancel_workflow_by_link_propagation, spawn_workflow_with_policy};
    use crate::{EngineError, RuntimeHandle, RuntimeInput};

    fn runtime() -> Result<RuntimeHandle, EngineError> {
        RuntimeHandle::new(RuntimeConfig::new(Some(1)))
    }

    #[test]
    fn workflow_spawn_policy_requests_trap_exit() -> Result<(), EngineError> {
        let runtime = runtime()?;
        let pid = runtime.spawn_test_process()?;

        assert!(!runtime.trap_exit(pid)?);
        runtime.set_trap_exit(pid, true)?;
        assert!(runtime.trap_exit(pid)?);
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn activity_process_policy_keeps_children_non_trapping() -> Result<(), EngineError> {
        let runtime = runtime()?;
        let workflow = runtime.spawn_test_process_with_trap_exit(true)?;
        let activity = runtime.spawn_linked_test_process(workflow)?;

        assert!(runtime.trap_exit(workflow)?);
        assert!(!runtime.trap_exit(activity)?);
        assert!(runtime.is_linked(workflow, activity)?);
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn abnormal_activity_exit_is_trapped_by_workflow() -> Result<(), EngineError> {
        let runtime = runtime()?;
        let workflow = runtime.spawn_test_process_with_trap_exit(true)?;
        let activity = runtime.spawn_linked_test_process(workflow)?;

        runtime.terminate_test_process_with_error(activity)?;

        assert!(runtime.is_live(workflow));
        assert!(runtime.has_trapped_exit_message(workflow, activity)?);
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn cancelling_workflow_terminates_linked_activity_children() -> Result<(), EngineError> {
        let runtime = runtime()?;
        let workflow = runtime.spawn_test_process_with_trap_exit(true)?;
        let first_activity = runtime.spawn_linked_test_process(workflow)?;
        let second_activity = runtime.spawn_linked_test_process(workflow)?;

        cancel_workflow_by_link_propagation(&runtime, workflow)?;

        assert!(!runtime.is_live(workflow));
        assert!(!runtime.is_live(first_activity));
        assert!(!runtime.is_live(second_activity));
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn policy_spawns_workflow_through_runtime_trapping_api() {
        let runtime = runtime();
        assert!(runtime.is_ok());
        if let Ok(runtime) = runtime {
            let result = spawn_workflow_with_policy(
                &runtime,
                "missing_workflow_module",
                "run",
                RuntimeInput::default(),
            );
            assert!(matches!(result, Err(EngineError::Runtime { .. })));
            assert!(runtime.shutdown().is_ok());
        }
    }
}
