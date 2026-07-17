use std::sync::Arc;

use aion_core::{ActivityError, ActivityErrorKind, ContentType, Payload};

use crate::error::EngineError;
use crate::runtime::config::RuntimeConfig;
use crate::runtime::handle::RuntimeHandle;
use crate::runtime::handle::activity_delivery::{ActivityOutcomeKind, RetainedActivityDelivery};

use super::TestResult;

fn test_runtime_error(reason: impl Into<String>) -> EngineError {
    EngineError::Runtime {
        reason: reason.into(),
    }
}

#[test]
fn failed_same_key_result_redelivery_restores_published_completion() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let workflow_pid = runtime.spawn_test_process()?;
    let key = (workflow_pid, 29);
    runtime.retain_activity_outcome_and_deliver_marker(
        workflow_pid,
        &runtime.activity_results,
        RetainedActivityDelivery {
            key,
            outcome: Payload::new(ContentType::Json, br#"{"original":true}"#.to_vec()),
            kind: ActivityOutcomeKind::Result,
            attempt: Some(2),
        },
        || Ok(()),
    )?;

    assert!(runtime.is_live(workflow_pid));
    let duplicate = runtime.retain_activity_outcome_and_deliver_marker(
        workflow_pid,
        &runtime.activity_results,
        RetainedActivityDelivery {
            key,
            outcome: Payload::new(ContentType::Json, br#"{"replacement":true}"#.to_vec()),
            kind: ActivityOutcomeKind::Result,
            attempt: Some(4),
        },
        || Err(test_runtime_error("forced duplicate marker rejection")),
    );

    assert!(duplicate.is_err());
    assert!(runtime.activity_delivery_index_contains_for_test(workflow_pid, 29)?);
    let (payload, attempt) = runtime
        .take_activity_result(workflow_pid, 29)?
        .ok_or("failed redelivery erased the published result")?;
    assert_eq!(payload.bytes(), br#"{"original":true}"#);
    assert_eq!(attempt, Some(2));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn failed_same_key_failure_redelivery_restores_published_completion() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(None))?);
    let workflow_pid = runtime.spawn_test_process()?;
    let key = (workflow_pid, 31);
    runtime.retain_activity_outcome_and_deliver_marker(
        workflow_pid,
        &runtime.activity_errors,
        RetainedActivityDelivery {
            key,
            outcome: ActivityError {
                kind: ActivityErrorKind::Terminal,
                message: "original failure".to_owned(),
                details: None,
            },
            kind: ActivityOutcomeKind::Error,
            attempt: Some(3),
        },
        || Ok(()),
    )?;

    assert!(runtime.is_live(workflow_pid));
    let duplicate = runtime.retain_activity_outcome_and_deliver_marker(
        workflow_pid,
        &runtime.activity_errors,
        RetainedActivityDelivery {
            key,
            outcome: ActivityError {
                kind: ActivityErrorKind::Terminal,
                message: "replacement failure".to_owned(),
                details: None,
            },
            kind: ActivityOutcomeKind::Error,
            attempt: Some(5),
        },
        || Err(test_runtime_error("forced duplicate marker rejection")),
    );

    assert!(duplicate.is_err());
    assert!(runtime.activity_delivery_index_contains_for_test(workflow_pid, 31)?);
    let (error, attempt) = runtime
        .take_activity_error(workflow_pid, 31)?
        .ok_or("failed redelivery erased the published failure")?;
    assert_eq!(error.message, "original failure");
    assert_eq!(attempt, Some(3));
    runtime.shutdown()?;
    Ok(())
}
