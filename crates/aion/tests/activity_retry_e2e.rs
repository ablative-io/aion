//! End-to-end tests for engine-honored per-activity retry policies (#197).
//!
//! The packaged `examples/retry-policy` workflow declares an explicit
//! `RetryPolicy` (3 total attempts, fixed backoff) on its one remote
//! activity and contains NO retry logic of its own. These tests prove, over
//! the real engine:
//!
//! 1. A worker that fails retryably N-1 times and then succeeds costs the
//!    run RETRIES, not the run: the workflow Completes, with the whole
//!    per-attempt trail recorded (`ActivityFailed` kind `Retryable` per
//!    failed attempt, `ActivityStarted` per delivery, terminal at
//!    `attempt = N`).
//! 2. A worker that exhausts the budget fails the workflow with the LAST
//!    reason verbatim and the final attempt count — and that run stays
//!    reopenable exactly as before: `reopen_workflow` re-drives the
//!    activity live at the NEXT attempt (the trail continues, never
//!    restarts) and the reopened run completes.
//!
//! The archive is rebuilt from the committed example source on every run —
//! see `common/example_build.rs` for why this gate must never skip.

#[path = "common/example_build.rs"]
mod example_build;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use aion::EngineBuilder;
use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion_core::{ActivityErrorKind, Event, Payload, WorkflowStatus};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

/// Deterministic stand-in for a remote worker whose backing service flakes:
/// fails every delivery with a retryable (transient) error until the wire
/// attempt reaches `succeed_from_attempt`, then replies normally. Records
/// each delivery's wire attempt for assertion.
struct FlakyDispatcher {
    succeed_from_attempt: u32,
    calls: std::sync::Mutex<Vec<u32>>,
    deliveries: AtomicU32,
}

impl FlakyDispatcher {
    fn new(succeed_from_attempt: u32) -> Arc<Self> {
        Arc::new(Self {
            succeed_from_attempt,
            calls: std::sync::Mutex::new(Vec::new()),
            deliveries: AtomicU32::new(0),
        })
    }

    fn seen_attempts(&self) -> Vec<u32> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

impl ActivityDispatcher for FlakyDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        if request.name != "flaky_call" {
            return Err(format!("terminal:unknown activity {}", request.name));
        }
        self.deliveries.fetch_add(1, Ordering::SeqCst);
        self.calls
            .lock()
            .map_err(|_| "terminal:calls lock poisoned".to_owned())?
            .push(request.attempt);
        if request.attempt < self.succeed_from_attempt {
            return Err(format!(
                "retryable:upstream stream reset (delivery attempt {})",
                request.attempt
            ));
        }
        let value: serde_json::Value = serde_json::from_str(&request.input)
            .map_err(|error| format!("terminal:bad input: {error}"))?;
        let who = value["name"].as_str().unwrap_or("stranger");
        Ok(json!({ "reply": format!("steady hello, {who}") }).to_string())
    }
}

async fn engine_with(
    dispatcher: Arc<FlakyDispatcher>,
    store: &Arc<dyn EventStore>,
) -> Result<aion::Engine, Box<dyn std::error::Error>> {
    let package = example_build::built_package("examples/retry-policy", "retry_policy")?;
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(dispatcher)
        .load_workflows(package)
        .build()
        .await?)
}

fn count_retryable_failures(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::ActivityFailed { error, .. } if error.kind == ActivityErrorKind::Retryable
            )
        })
        .count()
}

/// A provider flake must cost a retry, not a run: two transient failures
/// under a 3-attempt policy complete the workflow, with the full attempt
/// trail durably recorded.
#[tokio::test]
async fn retryable_flakes_cost_retries_not_the_run() -> Result<(), Box<dyn std::error::Error>> {
    let dispatcher = FlakyDispatcher::new(3);
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with(Arc::clone(&dispatcher), &store).await?;

    let handle = engine
        .start_workflow(
            "retry_policy",
            Payload::from_json(&json!({ "name": "Ada" }))?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    let reply: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(reply, json!("steady hello, Ada"));

    assert_eq!(
        dispatcher.seen_attempts(),
        vec![1, 2, 3],
        "each delivery must carry its incremented wire attempt"
    );

    let history = store.read_history(handle.workflow_id()).await?;
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Completed
    );
    assert_eq!(
        count_retryable_failures(&history),
        2,
        "both transient flakes must be recorded as non-terminal retryable failures: {history:#?}"
    );
    let started_attempts: Vec<u32> = history
        .iter()
        .filter_map(|event| match event {
            Event::ActivityStarted { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .collect();
    assert_eq!(
        started_attempts,
        vec![1, 2, 3],
        "every delivery records its ActivityStarted: {history:#?}"
    );
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::ActivityCompleted { attempt: 3, .. })),
        "the terminal completion must carry the final attempt: {history:#?}"
    );

    engine.shutdown()?;
    Ok(())
}

/// Exhausted budget: the workflow fails with the LAST retryable reason
/// verbatim and the final attempt count recorded on the terminal — and the
/// failed run reopens exactly as before, re-driving the activity live at the
/// NEXT attempt of the same trail.
#[tokio::test]
async fn exhausted_retries_fail_the_run_and_reopen_continues_the_trail()
-> Result<(), Box<dyn std::error::Error>> {
    // Succeeds only from attempt 4 — one past the policy budget of 3, so the
    // first run exhausts, and the reopened re-dispatch (attempt 4) succeeds.
    let dispatcher = FlakyDispatcher::new(4);
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_with(Arc::clone(&dispatcher), &store).await?;

    let handle = engine
        .start_workflow(
            "retry_policy",
            Payload::from_json(&json!({ "name": "Ada" }))?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    let error = match result {
        Err(error) => error,
        Ok(payload) => {
            return Err(format!("workflow must fail on an exhausted budget: {payload:?}").into());
        }
    };
    let detail = error
        .details
        .as_ref()
        .map(|payload| String::from_utf8_lossy(payload.bytes()).into_owned())
        .unwrap_or_default();
    assert!(
        detail.contains("upstream stream reset (delivery attempt 3)"),
        "the LAST attempt's reason must surface verbatim: {error:?}"
    );

    let history = store.read_history(handle.workflow_id()).await?;
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Failed
    );
    assert_eq!(count_retryable_failures(&history), 2, "{history:#?}");
    assert!(
        history.iter().any(|event| matches!(
            event,
            Event::ActivityFailed { error, attempt: 3, .. }
                if error.kind == ActivityErrorKind::Terminal
                    && error.message == "retryable:upstream stream reset (delivery attempt 3)"
        )),
        "the exhausted terminal must carry the final attempt and the verbatim reason: {history:#?}"
    );
    assert_eq!(dispatcher.seen_attempts(), vec![1, 2, 3]);

    // Reopen interplay: the exhausted run must stay reopenable exactly as
    // any failed run, and the reopened re-dispatch continues the attempt
    // trail at 4 (never re-uses an attempt identity).
    let reopened = engine
        .reopen_workflow(handle.workflow_id(), handle.run_id())
        .await?;
    let result = engine
        .result(reopened.workflow_id(), reopened.run_id())
        .await?;
    let payload = result.map_err(|error| format!("reopened run failed: {error:?}"))?;
    let reply: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    assert_eq!(reply, json!("steady hello, Ada"));
    assert_eq!(
        dispatcher.seen_attempts(),
        vec![1, 2, 3, 4],
        "the reopened re-dispatch must continue the wire attempt trail"
    );
    let history = store.read_history(handle.workflow_id()).await?;
    assert!(
        history
            .iter()
            .any(|event| matches!(event, Event::ActivityCompleted { attempt: 4, .. })),
        "the reopened completion must carry the continued attempt: {history:#?}"
    );
    assert_eq!(
        aion_core::status_from_events(&history),
        WorkflowStatus::Completed
    );

    engine.shutdown()?;
    Ok(())
}
