use super::support::*;

pub(in super::super) fn reenvelope(event: Event, workflow_id: &WorkflowId, seq: u64) -> Event {
    let envelope = EventEnvelope {
        seq,
        recorded_at: chrono::Utc::now(),
        workflow_id: workflow_id.clone(),
    };
    match event {
        Event::WorkflowStarted {
            workflow_type,
            input,
            run_id,
            parent_run_id,
            package_version,
            ..
        } => Event::WorkflowStarted {
            envelope,
            workflow_type,
            input,
            run_id,
            parent_run_id,
            package_version,
        },
        Event::ActivityScheduled {
            activity_id,
            activity_type,
            input,
            task_queue,
            node,
            ..
        } => Event::ActivityScheduled {
            envelope,
            activity_id,
            activity_type,
            input,
            task_queue,
            node,
        },
        Event::ActivityStarted {
            activity_id,
            attempt,
            ..
        } => Event::ActivityStarted {
            envelope,
            activity_id,
            attempt,
        },
        Event::ActivityCompleted {
            activity_id,
            result,
            attempt,
            ..
        } => Event::ActivityCompleted {
            envelope,
            activity_id,
            result,
            attempt,
        },
        Event::ActivityFailed {
            activity_id,
            error,
            attempt,
            ..
        } => Event::ActivityFailed {
            envelope,
            activity_id,
            error,
            attempt,
        },
        Event::ActivityCancelled {
            activity_id,
            attempt,
            ..
        } => Event::ActivityCancelled {
            envelope,
            activity_id,
            attempt,
        },
        Event::TimerFired { timer_id, .. } => Event::TimerFired { envelope, timer_id },
        Event::SearchAttributesUpdated {
            workflow_id: attribute_workflow_id,
            attributes,
            ..
        } => Event::SearchAttributesUpdated {
            envelope,
            workflow_id: attribute_workflow_id,
            attributes,
        },
        other => other,
    }
}

pub(in super::super) fn started_event(
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<Event, Box<dyn std::error::Error>> {
    Ok(Event::WorkflowStarted {
        envelope: EventEnvelope {
            seq: 1,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        },
        workflow_type: "collect-parent".to_owned(),
        input: Payload::from_json(&json!({ "fixture": "input" }))?,
        run_id: run_id.clone(),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    })
}

pub(in super::super) fn placeholder_envelope() -> EventEnvelope {
    EventEnvelope {
        seq: 0,
        recorded_at: chrono::Utc::now(),
        workflow_id: WorkflowId::new_v4(),
    }
}

pub(in super::super) fn scheduled_started(ordinal: u64, name: &str) -> Vec<Event> {
    vec![
        Event::ActivityScheduled {
            envelope: placeholder_envelope(),
            activity_id: ActivityId::from_sequence_position(ordinal),
            activity_type: name.to_owned(),
            input: Payload::new(ContentType::Json, br#""in""#.to_vec()),
            task_queue: String::from("default"),
            node: None,
        },
        Event::ActivityStarted {
            envelope: placeholder_envelope(),
            activity_id: ActivityId::from_sequence_position(ordinal),
            attempt: 1,
        },
    ]
}

pub(in super::super) fn completed(ordinal: u64, result: &str) -> Event {
    Event::ActivityCompleted {
        envelope: placeholder_envelope(),
        activity_id: ActivityId::from_sequence_position(ordinal),
        result: Payload::new(ContentType::Json, result.as_bytes().to_vec()),
        attempt: 1,
    }
}

pub(in super::super) fn scheduled_started_on(
    ordinal: u64,
    name: &str,
    task_queue: &str,
    node: Option<&str>,
) -> Vec<Event> {
    vec![
        Event::ActivityScheduled {
            envelope: placeholder_envelope(),
            activity_id: ActivityId::from_sequence_position(ordinal),
            activity_type: name.to_owned(),
            input: Payload::new(ContentType::Json, br#""in""#.to_vec()),
            task_queue: task_queue.to_owned(),
            node: node.map(str::to_owned),
        },
        Event::ActivityStarted {
            envelope: placeholder_envelope(),
            activity_id: ActivityId::from_sequence_position(ordinal),
            attempt: 1,
        },
    ]
}
