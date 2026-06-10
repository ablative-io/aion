use aion_store::Event;

pub(super) fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::WorkflowStarted { .. } => "WorkflowStarted",
        Event::WorkflowCompleted { .. } => "WorkflowCompleted",
        Event::WorkflowFailed { .. } => "WorkflowFailed",
        Event::WorkflowCancelled { .. } => "WorkflowCancelled",
        Event::WorkflowTimedOut { .. } => "WorkflowTimedOut",
        Event::WorkflowContinuedAsNew { .. } => "WorkflowContinuedAsNew",
        Event::SearchAttributesUpdated { .. } => "SearchAttributesUpdated",
        Event::ActivityScheduled { .. } => "ActivityScheduled",
        Event::ActivityStarted { .. } => "ActivityStarted",
        Event::ActivityCompleted { .. } => "ActivityCompleted",
        Event::ActivityFailed { .. } => "ActivityFailed",
        Event::ActivityCancelled { .. } => "ActivityCancelled",
        Event::TimerStarted { .. } => "TimerStarted",
        Event::TimerFired { .. } => "TimerFired",
        Event::TimerCancelled { .. } => "TimerCancelled",
        Event::WithTimeoutCompleted { .. } => "WithTimeoutCompleted",
        Event::SignalReceived { .. } => "SignalReceived",
        Event::SignalSent { .. } => "SignalSent",
        Event::ChildWorkflowStarted { .. } => "ChildWorkflowStarted",
        Event::ChildWorkflowCompleted { .. } => "ChildWorkflowCompleted",
        Event::ChildWorkflowFailed { .. } => "ChildWorkflowFailed",
        Event::ChildWorkflowCancelled { .. } => "ChildWorkflowCancelled",
        Event::ScheduleCreated { .. } => "ScheduleCreated",
        Event::ScheduleUpdated { .. } => "ScheduleUpdated",
        Event::SchedulePaused { .. } => "SchedulePaused",
        Event::ScheduleResumed { .. } => "ScheduleResumed",
        Event::ScheduleDeleted { .. } => "ScheduleDeleted",
        Event::ScheduleTriggered { .. } => "ScheduleTriggered",
    }
}

pub(super) fn queryable_flag(event: &Event) -> i64 {
    i64::from(matches!(
        event,
        Event::WorkflowStarted { .. }
            | Event::WorkflowCompleted { .. }
            | Event::WorkflowFailed { .. }
            | Event::WorkflowCancelled { .. }
            | Event::WorkflowTimedOut { .. }
            | Event::WorkflowContinuedAsNew { .. }
            | Event::ChildWorkflowStarted { .. }
    ))
}

pub(super) fn workflow_type(event: &Event) -> Option<&str> {
    match event {
        Event::WorkflowStarted { workflow_type, .. }
        | Event::ChildWorkflowStarted { workflow_type, .. } => Some(workflow_type.as_str()),
        _ => None,
    }
}

pub(super) fn child_workflow_id(event: &Event) -> Option<String> {
    match event {
        Event::ChildWorkflowStarted {
            child_workflow_id, ..
        } => Some(child_workflow_id.to_string()),
        _ => None,
    }
}
