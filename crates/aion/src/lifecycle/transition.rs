//! Registry-only suspend/resume residency transitions.
//!
//! This module owns only the active-registry residency flip for a workflow run.
//! AT invokes [`suspend`] when a workflow enters a durable wait, and invokes
//! [`resume`] when the awaited timer fires or a signal arrives. AD invokes
//! [`resume`] after replay or recovery re-creates a suspended workflow run.
//!
//! Durable timer scheduling, signal routing, replay/recovery, event appends,
//! runtime process management, and status projection are intentionally outside
//! this module. Suspend/resume never change [`aion_core::WorkflowStatus`]; a
//! suspended workflow remains `Running` according to the aion-core event
//! projection.

use aion_core::{RunId, WorkflowId};

use crate::EngineError;
use crate::registry::{Registry, Residency, WorkflowHandle};

/// Suspends an active workflow run in the registry without removing it.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] if the workflow run is absent, or
/// [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
pub fn suspend(
    registry: &Registry,
    id: &WorkflowId,
    run: &RunId,
) -> Result<WorkflowHandle, EngineError> {
    replace_residency(registry, id, run, Residency::Suspended)
}

/// Resumes an active workflow run in the registry.
///
/// Calling this for an already-resident handle is idempotent. Projected status
/// is not read or changed.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] if the workflow run is absent, or
/// [`EngineError::RegistryPoisoned`] if the registry lock was poisoned.
pub fn resume(
    registry: &Registry,
    id: &WorkflowId,
    run: &RunId,
) -> Result<WorkflowHandle, EngineError> {
    replace_residency(registry, id, run, Residency::Resident)
}

fn replace_residency(
    registry: &Registry,
    id: &WorkflowId,
    run: &RunId,
    residency: Residency,
) -> Result<WorkflowHandle, EngineError> {
    registry
        .replace_residency(id, run, residency)?
        .ok_or_else(|| EngineError::WorkflowNotFound {
            workflow_type: format!("{id}/{run}"),
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, EventEnvelope, Payload, PayloadError, WorkflowStatus};
    use aion_package::ContentHash;
    use chrono::Utc;
    use serde_json::json;

    use super::{resume, suspend};
    use crate::EngineError;
    use crate::registry::{
        CompletionNotifier, Registry, Residency, WorkflowHandle, WorkflowHandleParts,
    };

    type TestResult = Result<(), TestError>;

    #[derive(Debug)]
    enum TestError {
        Engine(EngineError),
        Payload(PayloadError),
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Engine(error) => write!(formatter, "{error}"),
                Self::Payload(error) => write!(formatter, "{error}"),
            }
        }
    }

    impl std::error::Error for TestError {}

    impl From<EngineError> for TestError {
        fn from(error: EngineError) -> Self {
            Self::Engine(error)
        }
    }

    impl From<PayloadError> for TestError {
        fn from(error: PayloadError) -> Self {
            Self::Payload(error)
        }
    }

    fn hash(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    fn handle(
        workflow_id: aion_core::WorkflowId,
        run_id: aion_core::RunId,
        status: WorkflowStatus,
    ) -> WorkflowHandle {
        let store = Arc::new(aion_store::InMemoryStore::default());
        let recorder = crate::durability::Recorder::new(workflow_id.clone(), store);
        WorkflowHandle::new(WorkflowHandleParts {
            workflow_id,
            run_id,
            pid: 42,
            workflow_type: "checkout".to_owned(),
            namespace: String::from("default"),
            loaded_version: hash(3),
            cached_status: status,
            residency: Residency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        })
    }

    fn envelope(workflow_id: &aion_core::WorkflowId, seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: workflow_id.clone(),
        }
    }

    fn payload(label: &str) -> Result<Payload, PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn started(workflow_id: &aion_core::WorkflowId) -> Result<Event, PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(workflow_id, 1),
            workflow_type: String::from("checkout"),
            input: payload("input")?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn insert_running_handle(
        registry: &Registry,
    ) -> Result<(aion_core::WorkflowId, aion_core::RunId), EngineError> {
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        registry.insert(
            (workflow_id.clone(), run_id.clone()),
            handle(workflow_id.clone(), run_id.clone(), WorkflowStatus::Running),
        )?;
        Ok((workflow_id, run_id))
    }

    #[test]
    fn suspend_keeps_running_handle_present_with_suspended_residency() -> TestResult {
        let registry = Registry::default();
        let (workflow_id, run_id) = insert_running_handle(&registry)?;

        let suspended = suspend(&registry, &workflow_id, &run_id)?;

        assert_eq!(suspended.residency(), Residency::Suspended);
        assert_eq!(suspended.cached_status(), WorkflowStatus::Running);
        let registered = registry.get(&workflow_id, &run_id)?;
        assert_eq!(
            registered.map(|handle| (handle.residency(), handle.cached_status())),
            Some((Residency::Suspended, WorkflowStatus::Running))
        );
        Ok(())
    }

    #[test]
    fn reconcile_after_suspend_preserves_suspended_residency() -> TestResult {
        let registry = Registry::default();
        let (workflow_id, run_id) = insert_running_handle(&registry)?;
        suspend(&registry, &workflow_id, &run_id)?;
        let events = vec![started(&workflow_id)?];

        let reconciled = registry.reconcile(&workflow_id, &run_id, &events)?;

        assert_eq!(
            reconciled.map(|handle| (handle.residency(), handle.cached_status())),
            Some((Residency::Suspended, WorkflowStatus::Running))
        );
        assert_eq!(
            registry
                .get(&workflow_id, &run_id)?
                .map(|handle| handle.residency()),
            Some(Residency::Suspended)
        );
        Ok(())
    }

    #[test]
    fn resume_returns_suspended_handle_to_resident_without_status_change() -> TestResult {
        let registry = Registry::default();
        let (workflow_id, run_id) = insert_running_handle(&registry)?;
        suspend(&registry, &workflow_id, &run_id)?;

        let resumed = resume(&registry, &workflow_id, &run_id)?;

        assert_eq!(resumed.residency(), Residency::Resident);
        assert_eq!(resumed.cached_status(), WorkflowStatus::Running);
        assert_eq!(
            registry
                .get(&workflow_id, &run_id)?
                .map(|handle| handle.residency()),
            Some(Residency::Resident)
        );
        Ok(())
    }

    #[test]
    fn resume_is_idempotent_for_already_resident_handle() -> TestResult {
        let registry = Registry::default();
        let (workflow_id, run_id) = insert_running_handle(&registry)?;

        let resumed = resume(&registry, &workflow_id, &run_id)?;

        assert_eq!(resumed.residency(), Residency::Resident);
        assert_eq!(resumed.cached_status(), WorkflowStatus::Running);
        Ok(())
    }

    #[test]
    fn suspend_missing_handle_returns_workflow_not_found() {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();

        assert!(matches!(
            suspend(&registry, &workflow_id, &run_id),
            Err(EngineError::WorkflowNotFound { .. })
        ));
    }

    #[test]
    fn resume_missing_handle_returns_workflow_not_found() {
        let registry = Registry::default();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();

        assert!(matches!(
            resume(&registry, &workflow_id, &run_id),
            Err(EngineError::WorkflowNotFound { .. })
        ));
    }
}
