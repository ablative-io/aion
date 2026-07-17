use super::*;

/// Retryable failure + budget left: the SAME ordinal re-dispatches at the
/// incremented attempt after the non-terminal failure and the retry start
/// are recorded — the observable per-attempt trail.
#[tokio::test]
async fn retryable_failure_redispatches_with_incremented_recorded_attempt() -> TestResult {
    let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
    let dispatcher = ScriptedRetryDispatcher::new(vec![
        Err("retryable:stream reset".to_owned()),
        Ok(r#""done""#.to_owned()),
    ]);
    let outcome = super::dispatch_with_retries(
        &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
        &harness.seam,
        &harness.request,
    )
    .await;

    assert!(
        matches!(
            &outcome.terminal,
            super::RetryLoopTerminal::Completed(payload) if payload == r#""done""#
        ),
        "the second attempt's success must be the delivered outcome"
    );
    assert_eq!(outcome.attempt, 2, "the completing attempt is attempt 2");
    assert_eq!(
        dispatcher.seen_attempts(),
        vec![1, 2],
        "the wire must carry the incremented attempt on the re-dispatch"
    );
    let history = harness.history().await?;
    assert!(
        matches!(
            history.get(3),
            Some(Event::ActivityFailed { error, attempt: 1, .. })
                if error.kind == aion_core::ActivityErrorKind::Retryable
                    && error.message == "retryable:stream reset"
        ),
        "the failed attempt must be recorded as a NON-terminal retryable failure: {history:#?}"
    );
    assert!(
        matches!(
            history.get(4),
            Some(Event::ActivityStarted { attempt: 2, .. })
        ),
        "the retry delivery must record its ActivityStarted: {history:#?}"
    );
    Ok(())
}

/// Exhausted budget: the loop stops at `max_attempts`, the LAST reason is
/// the delivered failure (verbatim), and the final attempt count rides
/// with it.
#[tokio::test]
async fn exhausted_retry_budget_fails_with_last_reason_and_attempt_count() -> TestResult {
    let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
    let dispatcher = ScriptedRetryDispatcher::new(vec![
        Err("retryable:reset one".to_owned()),
        Err("retryable:reset two".to_owned()),
        Err("retryable:reset three".to_owned()),
        Ok(r#""never delivered""#.to_owned()),
    ]);
    let outcome = super::dispatch_with_retries(
        &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
        &harness.seam,
        &harness.request,
    )
    .await;

    assert!(
        matches!(
            &outcome.terminal,
            super::RetryLoopTerminal::Failed(reason) if reason == "retryable:reset three"
        ),
        "the LAST reason must surface verbatim"
    );
    assert_eq!(outcome.attempt, 3, "the budget is total attempts");
    assert_eq!(
        dispatcher.seen_attempts(),
        vec![1, 2, 3],
        "exactly max_attempts deliveries, one per attempt"
    );
    let history = harness.history().await?;
    // Two recorded retryable failures (attempts 1 and 2) and two retry
    // starts (attempts 2 and 3); the THIRD failure is the delivered
    // terminal, recorded by the awaiting workflow, not the loop.
    let retryable_failures = history
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::ActivityFailed { error, .. }
                    if error.kind == aion_core::ActivityErrorKind::Retryable
            )
        })
        .count();
    assert_eq!(retryable_failures, 2, "{history:#?}");
    assert!(
        matches!(
            history.last(),
            Some(Event::ActivityStarted { attempt: 3, .. })
        ),
        "the final delivery's start must be recorded: {history:#?}"
    );
    Ok(())
}

/// Non-retryable failures behave exactly as before the retry loop:
/// one delivery, no recorded retry trail, the reason delivered verbatim.
#[tokio::test]
async fn non_retryable_failure_fails_immediately_without_a_retry_trail() -> TestResult {
    let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
    let dispatcher = ScriptedRetryDispatcher::new(vec![Err("terminal:bad request".to_owned())]);
    let outcome = super::dispatch_with_retries(
        &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
        &harness.seam,
        &harness.request,
    )
    .await;

    assert!(matches!(
        &outcome.terminal,
        super::RetryLoopTerminal::Failed(reason) if reason == "terminal:bad request"
    ));
    assert_eq!(outcome.attempt, 1);
    assert_eq!(dispatcher.seen_attempts(), vec![1]);
    assert_eq!(
        harness.history().await?.len(),
        3,
        "no retry events may be recorded for a non-retryable failure"
    );
    Ok(())
}

/// No declared policy (`"retry": null`) keeps the SDK's run-exactly-once
/// contract: a retryable-class failure is delivered after one attempt.
#[tokio::test]
async fn absent_policy_keeps_run_exactly_once_for_retryable_failures() -> TestResult {
    let harness = RetryLoopHarness::seeded(r#"{"retry":null}"#).await?;
    let dispatcher = ScriptedRetryDispatcher::new(vec![Err("retryable:stream reset".to_owned())]);
    let outcome = super::dispatch_with_retries(
        &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
        &harness.seam,
        &harness.request,
    )
    .await;

    assert!(matches!(
        &outcome.terminal,
        super::RetryLoopTerminal::Failed(reason) if reason == "retryable:stream reset"
    ));
    assert_eq!(dispatcher.seen_attempts(), vec![1]);
    assert_eq!(harness.history().await?.len(), 3);
    Ok(())
}
