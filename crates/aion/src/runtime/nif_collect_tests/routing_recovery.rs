use super::*;

/// NSTQ-3 recovery: when an in-flight dispatch is re-staged from history after a restart, the
/// re-armed item must re-target the SAME task queue the original `ActivityScheduled` recorded
/// (`(namespace, X)`), not silently fall back to `(namespace, "default")`. This is the durable
/// source-of-truth path the stale-recovery branch of `dispatch_unscheduled` consumes.
#[test]
fn recovery_re_targets_the_recorded_task_queue_not_default() -> TestResult {
    // History recorded the activity on task queue "claude" before the crash.
    let history = scheduled_started_on(0, "work", "claude", None);
    assert_eq!(scheduled_task_queue(&history, 0).as_deref(), Some("claude"));

    let work = spec("work");
    let members = [(0u64, &work)];
    let items = fan_out_items_recovered(&members, "remote", &history, "recovery")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].namespace, "remote",
        "recovery keeps the workflow namespace"
    );
    assert_eq!(
        items[0].task_queue, "claude",
        "recovery must re-target the RECORDED task queue, never the default"
    );
    Ok(())
}

/// NSTQ-3 recovery replay-safety: an OLD history (recorded before the `task_queue` field
/// existed) decodes its `ActivityScheduled` `task_queue` to the named default, so a recovered
/// dispatch deterministically re-targets `(namespace, "default")` — never panics, never differs.
#[test]
fn recovery_from_pre_field_history_defaults_task_queue() -> TestResult {
    // Reconstruct the exact old wire form: serialize a current event, strip task_queue, decode.
    let current = &scheduled_started_on(0, "work", "ignored-when-stripped", None)[0];
    let mut value = serde_json::to_value(current)?;
    let data = value
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("ActivityScheduled must serialize to a tagged object with a `data` map")?;
    assert!(data.remove("task_queue").is_some());
    let old_event: Event = serde_json::from_value(value)?;
    let history = vec![old_event];

    let work = spec("work");
    let members = [(0u64, &work)];
    let items = fan_out_items_recovered(&members, "remote", &history, "recovery")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

    assert_eq!(
        items[0].task_queue, "default",
        "an old history with no recorded task_queue must recover as the named default"
    );
    Ok(())
}

/// NODE-3 recovery: when an in-flight dispatch is re-staged from history after a restart, the
/// re-armed item must re-target the SAME node the original `ActivityScheduled` recorded, not
/// silently drop the affinity. This is the durable source-of-truth path the stale-recovery
/// branch of `dispatch_unscheduled` consumes.
#[test]
fn recovery_re_targets_the_recorded_node_not_none() -> TestResult {
    // History recorded the activity pinned to node "box-7" before the crash.
    let history = scheduled_started_on(0, "work", "claude", Some("box-7"));
    assert_eq!(scheduled_node(&history, 0).as_deref(), Some("box-7"));

    let work = spec("work");
    let members = [(0u64, &work)];
    let items = fan_out_items_recovered(&members, "remote", &history, "recovery")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].node.as_deref(),
        Some("box-7"),
        "recovery must re-target the RECORDED node, never silently drop affinity"
    );
    Ok(())
}

/// NODE-3 recovery replay-safety: an OLD history (recorded before the `node` field existed)
/// decodes its `ActivityScheduled` `node` to `None`, so a recovered dispatch deterministically
/// re-stages with no affinity — never a sentinel, never panics, never differs.
#[test]
fn recovery_from_pre_field_history_has_no_node() -> TestResult {
    // Reconstruct the exact old wire form: serialize a current event, strip node, decode.
    let current = &scheduled_started_on(0, "work", "claude", Some("ignored-when-stripped"))[0];
    let mut value = serde_json::to_value(current)?;
    let data = value
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("ActivityScheduled must serialize to a tagged object with a `data` map")?;
    assert!(data.remove("node").is_some());
    let old_event: Event = serde_json::from_value(value)?;
    let history = vec![old_event];
    assert_eq!(scheduled_node(&history, 0), None);

    let work = spec("work");
    let members = [(0u64, &work)];
    let items = fan_out_items_recovered(&members, "remote", &history, "recovery")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

    assert_eq!(
        items[0].node, None,
        "an old history with no recorded node must recover as no affinity (None)"
    );
    Ok(())
}
