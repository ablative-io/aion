use super::*;

/// The live expiry decision must be a pure function of the RESOLUTION
/// snapshot. Race modeled: the sweep's snapshot (a stale read) lacks the
/// scope deadline's `TimerFired`, which is recorded by the time any
/// later read runs. Before the fix `expired_scope_message` re-read the
/// store, saw the fired deadline, and cancelled the pending member on
/// the spot — an abort decided from events the resolution never
/// observed. After the fix the stale-snapshot pass suspends; the
/// deadline's wake re-enters with a fresh snapshot, cancels durably,
/// and a fresh engine epoch derives the identical abort from the
/// recorded set while appending nothing.
#[tokio::test(flavor = "multi_thread")]
async fn all_stale_snapshot_expiry_suspends_then_converges_with_replay() -> TestResult {
    let scope_timer = aion_core::TimerId::anonymous(7);
    // Stale snapshot = WorkflowStarted + batch (4) + Completed(0): the
    // deadline `TimerFired` (seq 7) is the one event past the window.
    let backing = Arc::new(crate::runtime::nif_test_stores::StaleReadStore::new(6));
    let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
    let mut events = pending_batch(&["a", "b"]);
    events.push(completed(0, r#""done-a""#));
    events.push(scope_deadline_fired(7));
    let (workflow_id, run_id) = seed_history(&store, &events).await?;
    let harness =
        CollectHarness::over_store(Arc::clone(&store), workflow_id.clone(), run_id.clone()).await?;
    install_fresh_read_bridge(&harness);
    backing.set_stale_target(&workflow_id, 1);
    // Live scope whose deadline is the recorded TimerFired(seq 7).
    harness
        .state
        .timeout_scopes
        .insert(21, TimeoutScope::live_for_test(harness.pid, scope_timer));
    harness
        .state
        .timeout_scope_stacks
        .insert(harness.pid, vec![21]);
    let two = specs(&["a", "b"]);

    // Pass 1 — stale resolution snapshot (no TimerFired): must suspend,
    // never decide the abort from a fresh read; nothing is cancelled.
    assert_eq!(
        harness.step(CollectKind::All, &two),
        Ok(CollectStep::Suspend),
        "a snapshot lacking the deadline must park, not branch"
    );
    assert_eq!(harness.cancelled_ordinals().await?, Vec::<u64>::new());

    // Pass 2 — fresh snapshot: the deadline is in the resolution read;
    // the unresolved member is cancelled durably and the await aborts.
    assert_eq!(
        harness.step(CollectKind::All, &two),
        Ok(CollectStep::ScopeExpired(
            "timeout:deadline expired".to_owned()
        ))
    );
    assert_eq!(harness.cancelled_ordinals().await?, vec![1]);
    assert_eq!(harness.pinned(), None);
    let history_len = store.read_history(&workflow_id).await?.len();
    harness.shutdown()?;

    // Fresh engine epoch over the final store (the restart analogue),
    // scope replay-derived expired exactly as `arm_scope` derives it:
    // the recorded cancellation set yields the same abort, appending
    // nothing.
    let replay = CollectHarness::over_store(store, workflow_id, run_id).await?;
    replay.state.timeout_scopes.insert(
        1,
        TimeoutScope::replayed_expired_with_deadline_for_test(
            replay.pid,
            aion_core::TimerId::anonymous(7),
        ),
    );
    replay
        .state
        .timeout_scope_stacks
        .insert(replay.pid, vec![1]);
    assert_eq!(
        replay.step(CollectKind::All, &two),
        Ok(CollectStep::ScopeExpired(
            "timeout:deadline expired".to_owned()
        )),
        "replay must take the same branch as the converged live run"
    );
    assert_eq!(
        replay.store.read_history(&replay.workflow_id).await?.len(),
        history_len,
        "replay must append nothing"
    );
    replay.shutdown()
}

/// `collect_race` twin of the stale-snapshot test: pre-fix, the fresh
/// read aborted the race on the spot — cancelling every member and
/// discarding the completion that was about to settle. Post-fix the
/// stale pass parks, and the wake re-entry settles the delivered
/// completion as the durably recorded winner — the branch a fresh
/// engine epoch reproduces from the recorded terminals alone.
#[tokio::test(flavor = "multi_thread")]
async fn race_stale_snapshot_expiry_suspends_then_settles_the_recorded_winner() -> TestResult {
    let scope_timer = aion_core::TimerId::anonymous(7);
    // Stale snapshot = WorkflowStarted + batch (4): the deadline
    // `TimerFired` (seq 6) is the one event past the window.
    let backing = Arc::new(crate::runtime::nif_test_stores::StaleReadStore::new(5));
    let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
    let mut events = pending_batch(&["a", "b"]);
    events.push(scope_deadline_fired(7));
    let (workflow_id, run_id) = seed_history(&store, &events).await?;
    let harness =
        CollectHarness::over_store(Arc::clone(&store), workflow_id.clone(), run_id.clone()).await?;
    install_fresh_read_bridge(&harness);
    backing.set_stale_target(&workflow_id, 1);
    harness
        .state
        .timeout_scopes
        .insert(23, TimeoutScope::live_for_test(harness.pid, scope_timer));
    harness
        .state
        .timeout_scope_stacks
        .insert(harness.pid, vec![23]);
    let two = specs(&["a", "b"]);

    // Pass 1 — stale snapshot: park; pre-fix the fresh read cancelled
    // both members here and returned ScopeExpired.
    assert_eq!(
        harness.step(CollectKind::Race, &two),
        Ok(CollectStep::Suspend),
        "a snapshot lacking the deadline must park, not branch"
    );
    assert_eq!(harness.cancelled_ordinals().await?, Vec::<u64>::new());

    // The race window's other arrival: member 0's completion lands in
    // the runtime maps before the wake re-entry.
    harness.deps.runtime.deliver_activity_completion_message(
        harness.pid,
        "activity:0",
        r#""r0""#.to_owned(),
    )?;

    // Pass 2 — fresh snapshot: the completion is taken and recorded as
    // the winner (winner-first is deterministic — the recorded terminal
    // IS the decision), the loser is cancelled durably.
    assert_eq!(
        harness.step(CollectKind::Race, &two),
        Ok(CollectStep::RaceWon(Ok(r#""r0""#.to_owned())))
    );
    assert_eq!(harness.cancelled_ordinals().await?, vec![1]);
    assert_eq!(harness.pinned(), None);
    let history_len = store.read_history(&workflow_id).await?.len();
    harness.shutdown()?;

    // Fresh engine epoch, scope replay-derived expired: the recorded
    // winner settles the race identically, appending nothing.
    let replay = CollectHarness::over_store(store, workflow_id, run_id).await?;
    replay.state.timeout_scopes.insert(
        1,
        TimeoutScope::replayed_expired_with_deadline_for_test(
            replay.pid,
            aion_core::TimerId::anonymous(7),
        ),
    );
    replay
        .state
        .timeout_scope_stacks
        .insert(replay.pid, vec![1]);
    assert_eq!(
        replay.step(CollectKind::Race, &two),
        Ok(CollectStep::RaceWon(Ok(r#""r0""#.to_owned()))),
        "replay must settle the recorded winner, not re-derive the race"
    );
    assert_eq!(
        replay.store.read_history(&replay.workflow_id).await?.len(),
        history_len,
        "replay must append nothing"
    );
    replay.shutdown()
}
