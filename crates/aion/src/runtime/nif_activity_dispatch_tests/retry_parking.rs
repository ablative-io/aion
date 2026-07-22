use super::*;

/// #207 park-beats-retry: the parked sentinel stands the loop down BEFORE
/// the retry-policy filter, even with budget left — no retry record, no
/// delivered failure, no consumed budget. The durable log keeps only the
/// seeded scheduled/started trail, byte-equivalent to a kill -9.
#[tokio::test]
async fn parked_dispatch_stands_down_without_recording_even_with_retry_budget() -> TestResult {
    let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
    let dispatcher =
        ScriptedRetryDispatcher::new(vec![Err(crate::runtime::PARKED_ACTIVITY_REASON.to_owned())]);
    let outcome = super::dispatch_with_retries(
        &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
        &harness.seam,
        &harness.request,
    )
    .await;

    assert!(
        matches!(outcome.terminal, super::RetryLoopTerminal::Parked),
        "a parked dispatch must stand down, never fail or retry"
    );
    assert_eq!(
        dispatcher.seen_attempts(),
        vec![1],
        "parking must not re-dispatch: no retry budget is consumed"
    );
    assert_eq!(
        harness.history().await?.len(),
        3,
        "nothing may be recorded for a parked dispatch — the durable log \
         must end at the dangling scheduled/started trail"
    );
    Ok(())
}

/// #207 sentinel is ephemeral end-to-end at the completion-task seam: a
/// parked dispatch delivers NOTHING to the workflow process (no completion,
/// no failure) and records nothing, so the process stays suspended for
/// restart recovery.
#[tokio::test(flavor = "multi_thread")]
async fn parked_dispatch_delivers_nothing_to_the_workflow_process() -> TestResult {
    let harness = RetryLoopHarness::seeded(r#"{"retry":null}"#).await?;
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let pid = runtime.spawn_test_process()?;
    let dispatcher =
        ScriptedRetryDispatcher::new(vec![Err(crate::runtime::PARKED_ACTIVITY_REASON.to_owned())]);
    spawn_completion_task(
        &tokio::runtime::Handle::current(),
        Arc::clone(&runtime),
        Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>,
        super::RetryRecorderSeam {
            recorder: Arc::clone(&harness.seam.recorder),
            run_id: harness.seam.run_id.clone(),
        },
        pid,
        super::correlation_id(0),
        harness.request.clone(),
    );
    // Give the completion task ample time to run to its terminal; a parked
    // dispatch must leave the runtime maps empty (nothing delivered).
    for _ in 0_u32..40 {
        if dispatcher.seen_attempts().len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        runtime.take_activity_result(pid, 0)?.is_none(),
        "a parked dispatch must never deliver a completion"
    );
    assert!(
        runtime.take_activity_error(pid, 0)?.is_none(),
        "the parked sentinel must never be delivered to workflow code"
    );
    assert_eq!(
        harness.history().await?.len(),
        3,
        "the durable log must be untouched by a parked dispatch"
    );
    runtime.shutdown()?;
    Ok(())
}

/// Settle race: a terminal recorded by another path (a `with_timeout`
/// expiry) while the loop runs must stop the loop — no retry record may
/// ever land after the ordinal's terminal.
#[tokio::test]
async fn retry_loop_aborts_without_recording_once_the_activity_settled() -> TestResult {
    let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
    // The workflow thread already recorded the ordinal's durable timeout
    // terminal (seq 4) before the loop's first failure comes back.
    let timeout_terminal = vec![Event::ActivityFailed {
        envelope: envelope(&harness.workflow_id, 4),
        activity_id: ActivityId::from_sequence_position(0),
        error: aion_core::ActivityError {
            kind: aion_core::ActivityErrorKind::Terminal,
            message: "timeout:deadline expired".to_owned(),
            details: None,
        },
        attempt: 1,
    }];
    harness
        .store
        .append(
            WriteToken::recorder(),
            &harness.workflow_id,
            &timeout_terminal,
            3,
        )
        .await?;
    let dispatcher = ScriptedRetryDispatcher::new(vec![Err("retryable:stream reset".to_owned())]);
    let outcome = super::dispatch_with_retries(
        &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
        &harness.seam,
        &harness.request,
    )
    .await;

    assert!(
        matches!(outcome.terminal, super::RetryLoopTerminal::SettledElsewhere),
        "the loop must observe the recorded terminal and stand down"
    );
    assert_eq!(
        harness.history().await?.len(),
        4,
        "nothing may be recorded after the ordinal's terminal"
    );
    Ok(())
}
