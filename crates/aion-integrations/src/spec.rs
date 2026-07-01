//! The neutral run identity handed to a harness at [`crate::AgentHarness::start`].

use aion_core::{ActivityId, Payload, WorkflowId};

/// The neutral identity + input for one agent run (one activity attempt).
///
/// Carries only what every harness needs to run an attempt: the `(workflow, activity, attempt)`
/// key and the input [`Payload`]. It names **no** harness-specific configuration — an adapter
/// holds any harness-specific settings itself (constructed when the [`crate::AgentHarness`] is
/// built), keeping this spec harness-blind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRunSpec {
    /// The workflow this activity attempt belongs to.
    pub workflow_id: WorkflowId,
    /// The activity within the workflow.
    pub activity_id: ActivityId,
    /// The attempt number of the activity being run.
    pub attempt: u32,
    /// The activity input handed to the agent.
    pub input: Payload,
}

impl AgentRunSpec {
    /// Builds a run spec from the neutral run identity and input payload.
    #[must_use]
    pub fn new(
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        attempt: u32,
        input: Payload,
    ) -> Self {
        Self {
            workflow_id,
            activity_id,
            attempt,
            input,
        }
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{ActivityId, ContentType, Payload, WorkflowId};

    use super::AgentRunSpec;

    #[test]
    fn spec_carries_run_identity_and_input() {
        let workflow_id = WorkflowId::new_v4();
        let activity_id = ActivityId::from_sequence_position(4);
        let input = Payload::new(ContentType::Json, b"{}".to_vec());

        let spec = AgentRunSpec::new(workflow_id.clone(), activity_id.clone(), 2, input.clone());

        assert_eq!(spec.workflow_id, workflow_id);
        assert_eq!(spec.activity_id, activity_id);
        assert_eq!(spec.attempt, 2);
        assert_eq!(spec.input, input);
    }
}
