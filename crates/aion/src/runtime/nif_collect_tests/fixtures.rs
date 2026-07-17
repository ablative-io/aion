fn failed(ordinal: u64, message: &str) -> Event {
    Event::ActivityFailed {
        envelope: placeholder_envelope(),
        activity_id: ActivityId::from_sequence_position(ordinal),
        error: ActivityError {
            kind: ActivityErrorKind::Terminal,
            message: message.to_owned(),
            details: None,
        },
        attempt: 1,
    }
}

fn spec(name: &str) -> ActivitySpec {
    ActivitySpec {
        name: name.to_owned(),
        input: r#""in""#.to_owned(),
        config: "{}".to_owned(),
    }
}

/// An [`ActivitySpec`] carrying the SDK's two task-queue selection fields in
/// its dispatch config: `task_queue` (the per-activity override) and
/// `workflow_task_queue` (the workflow-level default). `None` encodes the
/// SDK's "no selection" as JSON null.
fn spec_with_task_queue(
    name: &str,
    task_queue: Option<&str>,
    workflow_task_queue: Option<&str>,
) -> ActivitySpec {
    let field = |value: Option<&str>| match value {
        Some(text) => format!("\"{text}\""),
        None => "null".to_owned(),
    };
    ActivitySpec {
        name: name.to_owned(),
        input: r#""in""#.to_owned(),
        config: format!(
            r#"{{"labels":{{}},"task_queue":{},"workflow_task_queue":{}}}"#,
            field(task_queue),
            field(workflow_task_queue)
        ),
    }
}

/// An [`ActivitySpec`] carrying the SDK's OPTIONAL `node` affinity field in
/// its dispatch config. `None` encodes the SDK's "no pin" as JSON null.
fn spec_with_node(name: &str, node: Option<&str>) -> ActivitySpec {
    let field = match node {
        Some(text) => format!("\"{text}\""),
        None => "null".to_owned(),
    };
    ActivitySpec {
        name: name.to_owned(),
        input: r#""in""#.to_owned(),
        config: format!(
            r#"{{"labels":{{}},"task_queue":null,"workflow_task_queue":null,"node":{field}}}"#
        ),
    }
}

fn specs(names: &[&str]) -> Vec<ActivitySpec> {
    names.iter().map(|name| spec(name)).collect()
}

fn scope_deadline_fired(ordinal: u64) -> Event {
    Event::TimerFired {
        envelope: placeholder_envelope(),
        timer_id: aion_core::TimerId::anonymous(ordinal),
    }
}

/// Arm the per-test timer bridge that backed the OLD fresh-read expiry
/// path (`expired_scope_message` → `build_context_for_pid`); installing
/// it proves the stale-snapshot tests fail if a fresh read is
/// reintroduced, instead of accidentally passing because the fresh read
/// was unavailable.
fn install_fresh_read_bridge(harness: &CollectHarness) {
    crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
        &harness.state,
        Arc::clone(&harness.deps.registry),
        Arc::clone(&harness.store),
        tokio::runtime::Handle::current(),
        crate::runtime::SignalDeliveryConfig::default(),
    );
}

fn pending_batch(names: &[&str]) -> Vec<Event> {
    names
        .iter()
        .enumerate()
        .flat_map(|(ordinal, name)| {
            scheduled_started(u64::try_from(ordinal).unwrap_or(u64::MAX), name)
        })
        .collect()
}

