use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn pinned_base_is_reused_across_reentries_and_counter_advances_once() -> TestResult {
    let harness = CollectHarness::over_events(&pending_batch(&["alpha", "beta"])).await?;
    let two = specs(&["alpha", "beta"]);

    // First arrival: allocates the base once and pins it.
    assert_eq!(
        harness.step(CollectKind::All, &two),
        Ok(CollectStep::Suspend)
    );
    assert!(matches!(
        harness.pinned(),
        Some(PendingAwait::Collect {
            base_ordinal: 0,
            count: 2,
            kind: CollectKind::All,
        })
    ));
    assert_eq!(harness.handle.activity_ordinals_allocated(), 2);

    // Wake re-entry: the pinned base is reused, the counter must not
    // advance a second time.
    assert_eq!(
        harness.step(CollectKind::All, &two),
        Ok(CollectStep::Suspend)
    );
    assert!(matches!(
        harness.pinned(),
        Some(PendingAwait::Collect {
            base_ordinal: 0,
            ..
        })
    ));
    assert_eq!(harness.handle.activity_ordinals_allocated(), 2);
    harness.shutdown()
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_fast_returns_lowest_ordinal_failure_and_cancels_unresolved() -> TestResult {
    // Recorded failures at ordinals 1 and 2; ordinal 0 still pending.
    let mut events = pending_batch(&["a", "b", "c"]);
    events.push(failed(1, "boom-b"));
    events.push(failed(2, "boom-c"));
    let harness = CollectHarness::over_events(&events).await?;

    let step = harness.step(CollectKind::All, &specs(&["a", "b", "c"]));

    assert_eq!(step, Ok(CollectStep::FailFast("boom-b".to_owned())));
    assert_eq!(harness.cancelled_ordinals().await?, vec![0]);
    assert_eq!(harness.pinned(), None);
    harness.shutdown()
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_set_covers_exactly_the_unresolved_ordinals() -> TestResult {
    // 0 completed, 2 failed, 1 and 3 pending: the cancel set is {1, 3}.
    let mut events = pending_batch(&["a", "b", "c", "d"]);
    events.push(completed(0, r#""done-a""#));
    events.push(failed(2, "boom-c"));
    let harness = CollectHarness::over_events(&events).await?;

    let step = harness.step(CollectKind::All, &specs(&["a", "b", "c", "d"]));

    assert_eq!(step, Ok(CollectStep::FailFast("boom-c".to_owned())));
    assert_eq!(harness.cancelled_ordinals().await?, vec![1, 3]);
    assert_eq!(harness.pinned(), None);
    harness.shutdown()
}

#[tokio::test(flavor = "multi_thread")]
async fn race_winner_is_first_recorded_terminal_not_lowest_ordinal() -> TestResult {
    // Ordinal 1 settled first (its terminal is recorded); ordinal 0 is
    // still pending and must be cancelled, not preferred.
    let mut events = pending_batch(&["a", "b"]);
    events.push(completed(1, r#""win-b""#));
    let harness = CollectHarness::over_events(&events).await?;

    let step = harness.step(CollectKind::Race, &specs(&["a", "b"]));

    assert_eq!(step, Ok(CollectStep::RaceWon(Ok(r#""win-b""#.to_owned()))));
    assert_eq!(harness.cancelled_ordinals().await?, vec![0]);
    assert_eq!(harness.pinned(), None);

    // First-settle includes failure: a recorded failure wins the race.
    let mut events = pending_batch(&["a", "b"]);
    events.push(failed(1, "boom-b"));
    let failing = CollectHarness::over_events(&events).await?;
    assert_eq!(
        failing.step(CollectKind::Race, &specs(&["a", "b"])),
        Ok(CollectStep::RaceWon(Err("boom-b".to_owned())))
    );
    assert_eq!(failing.cancelled_ordinals().await?, vec![0]);
    harness.shutdown()?;
    failing.shutdown()
}

#[tokio::test(flavor = "multi_thread")]
async fn race_batch_tie_breaks_to_lowest_ordinal_and_drains_loser_entries() -> TestResult {
    let harness = CollectHarness::over_events(&pending_batch(&["a", "b"])).await?;
    // Both completions sit in the runtime maps on one wake.
    harness.deps.runtime.deliver_activity_completion_message(
        harness.pid,
        "activity:0",
        r#""r0""#.to_owned(),
    )?;
    harness.deps.runtime.deliver_activity_completion_message(
        harness.pid,
        "activity:1",
        r#""r1""#.to_owned(),
    )?;

    let step = harness.step(CollectKind::Race, &specs(&["a", "b"]));

    assert_eq!(step, Ok(CollectStep::RaceWon(Ok(r#""r0""#.to_owned()))));
    assert_eq!(harness.cancelled_ordinals().await?, vec![1]);
    // The loser's retained entry was dropped at settle (D5 hygiene).
    assert_eq!(harness.deps.runtime.retained_activity_completions(), 0);
    let history = harness.store.read_history(&harness.workflow_id).await?;
    let winner_terminals = history
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::ActivityCompleted { .. } | Event::ActivityFailed { .. }
            )
        })
        .count();
    assert_eq!(
        winner_terminals, 1,
        "exactly one non-cancelled terminal may exist: {history:#?}"
    );
    harness.shutdown()
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_list_resolves_immediately_without_pinning() -> TestResult {
    let harness = CollectHarness::over_events(&[]).await?;

    assert_eq!(
        harness.step(CollectKind::All, &[]),
        Ok(CollectStep::AllCompleted(Vec::new()))
    );
    assert_eq!(harness.pinned(), None);
    assert_eq!(harness.handle.activity_ordinals_allocated(), 0);

    let race = harness.step(CollectKind::Race, &[]);
    assert_eq!(race, Err("expected at least one activity".to_owned()));
    assert_eq!(harness.pinned(), None);
    harness.shutdown()
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_scope_cancels_unresolved_and_replay_derives_the_same_abort() -> TestResult {
    let mut events = pending_batch(&["a", "b"]);
    events.push(completed(0, r#""done-a""#));
    let harness = CollectHarness::over_events(&events).await?;
    harness
        .state
        .timeout_scopes
        .insert(9, TimeoutScope::replayed_for_test(harness.pid, true));
    harness
        .state
        .timeout_scope_stacks
        .insert(harness.pid, vec![9]);

    let step = harness.step(CollectKind::All, &specs(&["a", "b"]));

    assert_eq!(
        step,
        Ok(CollectStep::ScopeExpired(
            "timeout:deadline expired".to_owned()
        ))
    );
    assert_eq!(harness.cancelled_ordinals().await?, vec![1]);
    assert_eq!(harness.pinned(), None);
    let store = Arc::clone(&harness.store);
    let workflow_id = harness.workflow_id.clone();
    let run_id = harness.handle.run_id().clone();
    let history_len = store.read_history(&workflow_id).await?.len();
    harness.shutdown()?;

    // Fresh engine epoch over the same store (the restart analogue):
    // the recorded cancelled-without-failure set derives the same abort
    // and appends nothing.
    let replay = CollectHarness::over_store(store, workflow_id, run_id).await?;
    assert_eq!(
        replay.step(CollectKind::All, &specs(&["a", "b"])),
        Ok(CollectStep::ScopeExpired(
            "timeout:deadline expired".to_owned()
        ))
    );
    assert_eq!(
        replay.store.read_history(&replay.workflow_id).await?.len(),
        history_len,
        "replay must append nothing"
    );
    assert_eq!(replay.pinned(), None);
    replay.shutdown()
}
