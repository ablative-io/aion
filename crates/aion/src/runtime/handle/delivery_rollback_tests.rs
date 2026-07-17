use std::sync::Arc;
use std::time::Duration;

use aion_core::{ActivityErrorKind, ContentType, Payload};

use crate::runtime::config::{RuntimeConfig, SignalDeliveryConfig};
use crate::runtime::handle::RuntimeHandle;

use super::TestResult;

#[test]
fn first_live_marker_refusal_preserves_attempted_completion() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let workflow_pid = runtime.spawn_test_process()?;
    runtime.force_activity_marker_refusals_for_test(
        workflow_pid,
        27,
        runtime.signal_delivery().max_enqueue_attempts,
    );

    let delivery = runtime.deliver_activity_completion_message_with_attempt(
        workflow_pid,
        "activity:27",
        r#"{"fast":true}"#.to_owned(),
        Some(6),
    );

    assert!(delivery.is_err());
    assert!(runtime.is_live(workflow_pid));
    assert!(runtime.activity_delivery_index_contains_for_test(workflow_pid, 27)?);
    let (payload, attempt) = runtime
        .take_activity_result(workflow_pid, 27)?
        .ok_or("live marker refusal erased the first completion")?;
    assert_eq!(payload.bytes(), br#"{"fast":true}"#);
    assert_eq!(attempt, Some(6));
    assert_clean_delivery_index(&runtime, workflow_pid, 27)?;
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn suspended_workflow_marker_retries_after_live_refusal() -> TestResult {
    let delivery =
        SignalDeliveryConfig::new(Duration::from_millis(50), 3, Duration::ZERO, Duration::ZERO);
    let runtime = Arc::new(RuntimeHandle::new(
        RuntimeConfig::new(None).with_signal_delivery(delivery),
    )?);
    let workflow_pid = runtime.spawn_test_process()?;
    runtime.force_activity_marker_refusals_for_test(workflow_pid, 28, 1);

    runtime.deliver_activity_completion_message_with_attempt(
        workflow_pid,
        "activity:28",
        r#"{"retried":true}"#.to_owned(),
        Some(7),
    )?;

    let (payload, attempt) = runtime
        .take_activity_result(workflow_pid, 28)?
        .ok_or("successful marker retry did not retain the completion")?;
    assert_eq!(payload.bytes(), br#"{"retried":true}"#);
    assert_eq!(attempt, Some(7));
    assert_clean_delivery_index(&runtime, workflow_pid, 28)?;
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn failed_completion_wrapper_redelivery_restores_published_completion() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let workflow_pid = runtime.spawn_test_process()?;
    runtime.deliver_activity_completion_message_with_attempt(
        workflow_pid,
        "activity:29",
        r#"{"original":true}"#.to_owned(),
        Some(2),
    )?;
    runtime.force_activity_marker_refusals_for_test(
        workflow_pid,
        29,
        runtime.signal_delivery().max_enqueue_attempts,
    );

    let duplicate = runtime.deliver_activity_completion_message_with_attempt(
        workflow_pid,
        "activity:29",
        r#"{"replacement":true}"#.to_owned(),
        Some(4),
    );

    assert!(duplicate.is_err());
    assert!(runtime.activity_delivery_index_contains_for_test(workflow_pid, 29)?);
    let (payload, attempt) = runtime
        .take_activity_result(workflow_pid, 29)?
        .ok_or("failed redelivery erased the published result")?;
    assert_eq!(payload.bytes(), br#"{"original":true}"#);
    assert_eq!(attempt, Some(2));
    assert_clean_delivery_index(&runtime, workflow_pid, 29)?;
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn failed_failure_wrapper_redelivery_restores_published_failure() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let workflow_pid = runtime.spawn_test_process()?;
    runtime.deliver_activity_failure_message_with_attempt(
        workflow_pid,
        "activity:31",
        "original failure".to_owned(),
        Some(3),
    )?;
    runtime.force_activity_marker_refusals_for_test(
        workflow_pid,
        31,
        runtime.signal_delivery().max_enqueue_attempts,
    );

    let duplicate = runtime.deliver_activity_failure_message_with_attempt(
        workflow_pid,
        "activity:31",
        "replacement failure".to_owned(),
        Some(5),
    );

    assert!(duplicate.is_err());
    assert!(runtime.activity_delivery_index_contains_for_test(workflow_pid, 31)?);
    let (error, attempt) = runtime
        .take_activity_error(workflow_pid, 31)?
        .ok_or("failed redelivery erased the published failure")?;
    assert_eq!(error.kind, ActivityErrorKind::Terminal);
    assert_eq!(error.message, "original failure");
    assert_eq!(error.details, None);
    assert_eq!(attempt, Some(3));
    assert_clean_delivery_index(&runtime, workflow_pid, 31)?;
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn failed_legacy_result_redelivery_restores_published_completion() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let workflow_pid = runtime.spawn_test_process()?;
    let original = Payload::new(ContentType::Json, br#"{"legacy":"a"}"#.to_vec());
    runtime.deliver_activity_result(workflow_pid, 33, original)?;
    runtime.force_activity_marker_refusals_for_test(
        workflow_pid,
        33,
        runtime.signal_delivery().max_enqueue_attempts,
    );

    let duplicate = runtime.deliver_activity_result(
        workflow_pid,
        33,
        Payload::new(ContentType::Json, br#"{"legacy":"b"}"#.to_vec()),
    );

    assert!(duplicate.is_err());
    assert!(runtime.activity_delivery_index_contains_for_test(workflow_pid, 33)?);
    let (payload, attempt) = runtime
        .take_activity_result(workflow_pid, 33)?
        .ok_or("failed legacy redelivery erased the published result")?;
    assert_eq!(payload.bytes(), br#"{"legacy":"a"}"#);
    assert_eq!(attempt, None);
    assert_clean_delivery_index(&runtime, workflow_pid, 33)?;
    runtime.shutdown()?;
    Ok(())
}

fn assert_clean_delivery_index(
    runtime: &RuntimeHandle,
    workflow_pid: u64,
    activity_sequence: u64,
) -> TestResult {
    assert!(!runtime.activity_delivery_index_contains_for_test(workflow_pid, activity_sequence,)?);
    assert_eq!(runtime.retained_activity_completions(), 0);
    assert_eq!(runtime.retained_activity_attempt_count_for_test(), 0);
    Ok(())
}
