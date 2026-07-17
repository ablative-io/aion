use super::*;

/// NSTQ-4 fresh dispatch: a fan-out member whose config selects task queue
/// "claude" produces an outbox item (the durable row) on "claude", not the
/// named default. This is the host-decode → outbox-row seam.
#[test]
fn fresh_fan_out_item_carries_the_selected_task_queue() -> TestResult {
    let claude = spec_with_task_queue("work", Some("claude"), None);
    let members = [(0u64, &claude)];
    let items = fan_out_items(&members, "remote", None, "fanout")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].namespace, "remote");
    assert_eq!(
        items[0].task_queue, "claude",
        "the member override must reach the fresh outbox row"
    );
    Ok(())
}

/// NSTQ-4 precedence at the fresh-dispatch seam: a member with no override
/// under a workflow defaulting to "gpu" resolves to "gpu"; with neither (and
/// no start-time queue), to the named default. Mixed members each land on
/// their own resolved queue.
#[test]
fn fresh_fan_out_items_resolve_precedence_per_member() -> TestResult {
    let overridden = spec_with_task_queue("a", Some("claude"), Some("gpu"));
    let defaulted = spec_with_task_queue("b", None, Some("gpu"));
    let plain = spec_with_task_queue("c", None, None);
    let members = [(0u64, &overridden), (1u64, &defaulted), (2u64, &plain)];
    let items = fan_out_items(&members, "remote", None, "fanout")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

    let queues: Vec<&str> = items.iter().map(|i| i.task_queue.as_str()).collect();
    assert_eq!(
        queues,
        vec!["claude", "gpu", aion_core::DEFAULT_TASK_QUEUE],
        "override > workflow default > the named default, resolved once per member"
    );
    Ok(())
}

/// #144 precedence at the fresh-dispatch seam: under a workflow STARTED on
/// "started-on", a member with an explicit override keeps it, a member with
/// only the SDK-declared workflow default keeps that, and a member that
/// selects neither falls back to the workflow's start-time queue — NOT the
/// named default. The start-time queue threads in once for the whole batch.
#[test]
fn fresh_fan_out_items_fall_back_to_the_start_time_queue() -> TestResult {
    let overridden = spec_with_task_queue("a", Some("claude"), None);
    let defaulted = spec_with_task_queue("b", None, Some("gpu"));
    let plain = spec_with_task_queue("c", None, None);
    let members = [(0u64, &overridden), (1u64, &defaulted), (2u64, &plain)];
    let items = fan_out_items(&members, "remote", Some("started-on"), "fanout")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

    let queues: Vec<&str> = items.iter().map(|i| i.task_queue.as_str()).collect();
    assert_eq!(
        queues,
        vec!["claude", "gpu", "started-on"],
        "override > workflow default > the recorded start-time queue (never the named default)"
    );
    Ok(())
}

/// NODE-4 fresh dispatch: a fan-out member whose config pins node "box-7"
/// produces an outbox item (the durable row) carrying node=Some("box-7"); a
/// member with no pin carries node=None. This is the host-decode → outbox-row
/// seam for the OPTIONAL affinity.
#[test]
fn fresh_fan_out_item_carries_the_selected_node() -> TestResult {
    let pinned = spec_with_node("a", Some("box-7"));
    let unpinned = spec_with_node("b", None);
    let members = [(0u64, &pinned), (1u64, &unpinned)];
    let items = fan_out_items(&members, "remote", None, "fanout")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;

    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0].node.as_deref(),
        Some("box-7"),
        "the member pin must reach the fresh outbox row"
    );
    assert_eq!(
        items[1].node, None,
        "an unpinned member must carry no affinity"
    );
    Ok(())
}

/// NODE-4 end-to-end through the flag-OFF schedule path: scheduling a batch
/// mixing a node-pinned member and an unpinned member records each
/// `ActivityScheduled` with its resolved node (host decode → recorder →
/// durable history).
#[tokio::test(flavor = "multi_thread")]
async fn scheduled_events_record_each_members_resolved_node() -> TestResult {
    let mut harness = CollectHarness::over_events(&[]).await?;
    harness.deps.dispatcher = Some(Arc::new(NeverDispatcher));
    let specs = vec![
        spec_with_node("a", Some("box-7")),
        spec_with_node("b", None),
    ];

    assert_eq!(
        harness.step(CollectKind::All, &specs),
        Ok(CollectStep::Suspend)
    );

    assert_eq!(
        harness.scheduled_nodes().await?,
        vec![(0, Some("box-7".to_owned())), (1, None)],
        "each recorded ActivityScheduled must carry its resolved node affinity"
    );
    harness.shutdown()
}

/// NSTQ-4 end-to-end through the flag-OFF schedule path: scheduling a batch
/// that mixes a "claude"-selected member with a workflow-"gpu"-defaulted
/// member and a no-selection member records each `ActivityScheduled` on its
/// own resolved task queue (host decode → recorder → durable history).
#[tokio::test(flavor = "multi_thread")]
async fn scheduled_events_record_each_members_resolved_task_queue() -> TestResult {
    let mut harness = CollectHarness::over_events(&[]).await?;
    // A dispatcher that never completes: the fresh batch's Scheduled+Started
    // events are recorded durably up-front, then the step parks at Suspend
    // (no completion arrives), so the recorded task queues are observable
    // without racing a settlement.
    harness.deps.dispatcher = Some(Arc::new(NeverDispatcher));
    let specs = vec![
        spec_with_task_queue("a", Some("claude"), Some("gpu")),
        spec_with_task_queue("b", None, Some("gpu")),
        spec_with_task_queue("c", None, None),
    ];

    assert_eq!(
        harness.step(CollectKind::All, &specs),
        Ok(CollectStep::Suspend)
    );

    assert_eq!(
        harness.scheduled_task_queues().await?,
        vec![
            (0, "claude".to_owned()),
            (1, "gpu".to_owned()),
            (2, aion_core::DEFAULT_TASK_QUEUE.to_owned()),
        ],
        "each recorded ActivityScheduled must carry its resolved task queue"
    );
    harness.shutdown()
}

/// A `SearchAttributesUpdated` event recording the workflow's start-time
/// task queue as the `aion.task_queue` attribute — exactly as the server
/// stamps it in the same append as `WorkflowStarted` (#144). This is the
/// durable, history-resident source the start-time-queue fallback reads.
fn start_time_task_queue_event(queue: &str) -> Event {
    Event::SearchAttributesUpdated {
        envelope: placeholder_envelope(),
        workflow_id: WorkflowId::new_v4(),
        attributes: std::collections::HashMap::from([(
            aion_core::START_TIME_TASK_QUEUE_ATTRIBUTE.to_owned(),
            aion_core::SearchAttributeValue::String(queue.to_owned()),
        )]),
    }
}

/// #144 end-to-end through the flag-OFF schedule path: a workflow STARTED on
/// "started-on" (recorded as the `aion.task_queue` search attribute) that
/// fans out a member selecting NO task queue anywhere records its
/// `ActivityScheduled` on "started-on", NOT the named default — the
/// previously-silent fallback. The start-time queue is read from recorded
/// history, so it is replay-stable.
#[tokio::test(flavor = "multi_thread")]
async fn no_selection_records_on_the_workflow_start_time_queue() -> TestResult {
    let harness = CollectHarness::over_events(&[start_time_task_queue_event("started-on")]).await?;
    let mut harness = harness;
    harness.deps.dispatcher = Some(Arc::new(NeverDispatcher));
    // One member with no override and no workflow declared default.
    let specs = vec![spec_with_task_queue("a", None, None)];

    assert_eq!(
        harness.step(CollectKind::All, &specs),
        Ok(CollectStep::Suspend)
    );

    assert_eq!(
        harness.scheduled_task_queues().await?,
        vec![(0, "started-on".to_owned())],
        "a no-selection activity must record on the workflow's start-time queue, not default"
    );
    harness.shutdown()
}

/// #144 replay-stability: recovery (a fresh engine epoch over the same store)
/// re-resolves a no-selection activity to the SAME recorded start-time queue.
/// The first epoch schedules the activity on "started-on" and parks; a fresh
/// epoch replays the same collect over the recorded history and the recorded
/// `ActivityScheduled` still reads back "started-on" — never re-defaulting,
/// never diverging run-to-run. Mirrors `recovery_re_targets_the_recorded_*`.
#[tokio::test(flavor = "multi_thread")]
async fn recovery_re_resolves_the_start_time_queue_not_default() -> TestResult {
    // Epoch 1: schedule the no-selection member on the start-time queue.
    let first = CollectHarness::over_events(&[start_time_task_queue_event("started-on")]).await?;
    let mut first = first;
    first.deps.dispatcher = Some(Arc::new(NeverDispatcher));
    let specs = vec![spec_with_task_queue("a", None, None)];
    assert_eq!(
        first.step(CollectKind::All, &specs),
        Ok(CollectStep::Suspend)
    );
    let recorded_first = first.scheduled_task_queues().await?;
    assert_eq!(recorded_first, vec![(0, "started-on".to_owned())]);
    let store = Arc::clone(&first.store);
    let workflow_id = first.workflow_id.clone();
    let run_id = first.handle.run_id().clone();
    first.shutdown()?;

    // Epoch 2 (the restart analogue): a fresh registry/runtime/ordinal
    // counter over the SAME store replays the recorded history.
    let replay = CollectHarness::over_store(store, workflow_id, run_id).await?;
    let mut replay = replay;
    replay.deps.dispatcher = Some(Arc::new(NeverDispatcher));
    assert_eq!(
        replay.step(CollectKind::All, &specs),
        Ok(CollectStep::Suspend),
        "replay must re-enter the same pending collect"
    );
    assert_eq!(
        replay.scheduled_task_queues().await?,
        vec![(0, "started-on".to_owned())],
        "replay must re-resolve to the recorded start-time queue, never the default"
    );
    replay.shutdown()
}
