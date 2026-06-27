//! Opt-in per-run activity mocking, layered over the production dispatcher.
//!
//! The mock is a thin decorator on the real [`aion::ActivityDispatcher`]: a
//! [`DevMockingDispatcher`] wraps the production [`WorkerActivityDispatcher`]
//! and consults a shared [`ActivityMockRegistry`] before every dispatch. When a
//! mock is registered for the dispatch's `(workflow_id, activity_name)`, the
//! canned result is returned and the real worker is never contacted; otherwise
//! the dispatch is delegated to the wrapped dispatcher unchanged.
//!
//! This deliberately changes nothing in the engine (CN4): the engine still
//! schedules, records, and replays the activity through its single Recorder
//! exactly as in production — the recorded `ActivityCompleted` event for a
//! mocked activity is indistinguishable from a real worker's completion, so a
//! replay or a server restart re-drives the run identically. The mock only
//! short-circuits the transport-side worker round-trip for one run.
//!
//! Mocks are keyed by the *real* [`WorkflowId`] the engine recorded, so they
//! scope to exactly the run a developer triggered (each dev trigger starts a
//! fresh workflow id) and never leak across runs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aion::{ActivityDispatch, ActivityDispatcher};
use aion_core::WorkflowId;

/// A canned activity outcome installed for one workflow run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MockedActivity {
    /// The activity returns this JSON-encoded result string verbatim — the same
    /// shape a real worker reports on the success side of the FFI contract.
    Succeeds {
        /// JSON-encoded typed result returned to the workflow.
        result_json: String,
    },
    /// The activity fails with this error message — the failure side of the FFI
    /// contract the SDK decodes.
    Fails {
        /// Human-readable failure message returned to the workflow.
        message: String,
    },
}

/// Key identifying a mock: a specific activity name within a specific run.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MockKey {
    workflow_id: WorkflowId,
    activity_name: String,
}

/// Shared, mutable registry of per-run activity mocks consulted on every
/// dispatch. Cloned freely; all clones share one table.
#[derive(Clone, Default)]
pub struct ActivityMockRegistry {
    mocks: Arc<Mutex<HashMap<MockKey, MockedActivity>>>,
}

impl std::fmt::Debug for ActivityMockRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActivityMockRegistry")
            .finish_non_exhaustive()
    }
}

impl ActivityMockRegistry {
    /// Build an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a mock for `activity_name` within `workflow_id`, replacing any
    /// previously registered mock for the same pair.
    ///
    /// # Errors
    ///
    /// Returns the lock-poison message when the registry mutex was poisoned by
    /// a panic in another holder.
    pub fn register(
        &self,
        workflow_id: WorkflowId,
        activity_name: impl Into<String>,
        mock: MockedActivity,
    ) -> Result<(), String> {
        let key = MockKey {
            workflow_id,
            activity_name: activity_name.into(),
        };
        self.mocks
            .lock()
            .map_err(|_| "activity mock registry mutex poisoned".to_owned())?
            .insert(key, mock);
        Ok(())
    }

    /// Look up the mock for a dispatch, if one is installed.
    ///
    /// # Errors
    ///
    /// Returns the lock-poison message when the registry mutex was poisoned.
    fn lookup(
        &self,
        workflow_id: &WorkflowId,
        activity_name: &str,
    ) -> Result<Option<MockedActivity>, String> {
        let guard = self
            .mocks
            .lock()
            .map_err(|_| "activity mock registry mutex poisoned".to_owned())?;
        // The key borrows owned fields, so build it from the lookup inputs.
        Ok(guard
            .get(&MockKey {
                workflow_id: workflow_id.clone(),
                activity_name: activity_name.to_owned(),
            })
            .cloned())
    }

    /// Whether any mock is currently registered for `workflow_id`.
    ///
    /// # Errors
    ///
    /// Returns the lock-poison message when the registry mutex was poisoned.
    pub fn has_any_for(&self, workflow_id: &WorkflowId) -> Result<bool, String> {
        let guard = self
            .mocks
            .lock()
            .map_err(|_| "activity mock registry mutex poisoned".to_owned())?;
        Ok(guard.keys().any(|key| &key.workflow_id == workflow_id))
    }
}

/// Production dispatcher wrapped with per-run activity mocking.
///
/// Installed in place of the bare [`WorkerActivityDispatcher`] only when the
/// dev surface is commissioned. With no mock registered for a dispatch it is a
/// transparent pass-through, so a server running the dev surface but with no
/// active mocks behaves exactly as production.
pub struct DevMockingDispatcher {
    inner: Arc<dyn ActivityDispatcher>,
    registry: ActivityMockRegistry,
}

impl std::fmt::Debug for DevMockingDispatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DevMockingDispatcher")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl DevMockingDispatcher {
    /// Wrap `inner` with the shared mock registry.
    #[must_use]
    pub fn new(inner: Arc<dyn ActivityDispatcher>, registry: ActivityMockRegistry) -> Self {
        Self { inner, registry }
    }
}

impl ActivityDispatcher for DevMockingDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        match self.registry.lookup(&request.workflow_id, &request.name)? {
            Some(MockedActivity::Succeeds { result_json }) => {
                tracing::info!(
                    operation = "dev.activity_mock",
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    activity_type = %request.name,
                    outcome = "succeeded",
                    "dev activity mock returned a canned result"
                );
                Ok(result_json)
            }
            Some(MockedActivity::Fails { message }) => {
                tracing::info!(
                    operation = "dev.activity_mock",
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    activity_type = %request.name,
                    outcome = "failed",
                    "dev activity mock returned a canned failure"
                );
                Err(message)
            }
            None => self.inner.dispatch(request),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use aion::{ActivityDispatch, ActivityDispatcher};
    use aion_core::{ActivityId, WorkflowId};

    use super::{ActivityMockRegistry, DevMockingDispatcher, MockedActivity};

    /// Inner dispatcher that records every delegated call and echoes its input.
    #[derive(Default)]
    struct RecordingInner {
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl ActivityDispatcher for RecordingInner {
        fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
            self.calls
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .push(request.name.clone());
            Ok(format!("real:{}", request.input))
        }
    }

    fn dispatch(workflow_id: WorkflowId, name: &str) -> ActivityDispatch {
        ActivityDispatch {
            namespace: "default".to_owned(),
            task_queue: "default".to_owned(),
            workflow_id,
            activity_id: ActivityId::from_sequence_position(0),
            name: name.to_owned(),
            input: "{}".to_owned(),
            config: "{}".to_owned(),
            attempt: 1,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn mocked_activity_returns_canned_result_without_delegating() -> Result<(), String> {
        let inner = Arc::new(RecordingInner::default());
        let registry = ActivityMockRegistry::new();
        let workflow_id = WorkflowId::new_v4();
        registry.register(
            workflow_id.clone(),
            "charge-card",
            MockedActivity::Succeeds {
                result_json: r#"{"charged":true}"#.to_owned(),
            },
        )?;
        let dispatcher = DevMockingDispatcher::new(inner.clone(), registry);

        let result = dispatcher.dispatch(dispatch(workflow_id, "charge-card"));

        assert_eq!(result, Ok(r#"{"charged":true}"#.to_owned()));
        assert!(
            inner
                .calls
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .is_empty(),
            "a mocked activity must not reach the real dispatcher"
        );
        Ok(())
    }

    #[test]
    fn mocked_failure_short_circuits_with_the_canned_message() -> Result<(), String> {
        let inner = Arc::new(RecordingInner::default());
        let registry = ActivityMockRegistry::new();
        let workflow_id = WorkflowId::new_v4();
        registry.register(
            workflow_id.clone(),
            "charge-card",
            MockedActivity::Fails {
                message: "card declined".to_owned(),
            },
        )?;
        let dispatcher = DevMockingDispatcher::new(inner, registry);

        assert_eq!(
            dispatcher.dispatch(dispatch(workflow_id, "charge-card")),
            Err("card declined".to_owned())
        );
        Ok(())
    }

    #[test]
    fn unmocked_activity_delegates_to_the_real_dispatcher() -> Result<(), String> {
        let inner = Arc::new(RecordingInner::default());
        let registry = ActivityMockRegistry::new();
        let dispatcher = DevMockingDispatcher::new(inner.clone(), registry);

        let result = dispatcher.dispatch(dispatch(WorkflowId::new_v4(), "ship-order"));

        assert_eq!(result, Ok("real:{}".to_owned()));
        assert_eq!(
            inner
                .calls
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .as_slice(),
            ["ship-order"]
        );
        Ok(())
    }

    #[test]
    fn mock_is_scoped_to_its_workflow_run() -> Result<(), String> {
        let inner = Arc::new(RecordingInner::default());
        let registry = ActivityMockRegistry::new();
        let mocked = WorkflowId::new_v4();
        let other = WorkflowId::new_v4();
        registry.register(
            mocked.clone(),
            "charge-card",
            MockedActivity::Succeeds {
                result_json: r#"{"charged":true}"#.to_owned(),
            },
        )?;
        let dispatcher = DevMockingDispatcher::new(inner.clone(), registry.clone());

        // The same activity name on a different run is NOT mocked.
        assert_eq!(
            dispatcher.dispatch(dispatch(other.clone(), "charge-card")),
            Ok("real:{}".to_owned())
        );
        assert!(registry.has_any_for(&mocked)?);
        assert!(!registry.has_any_for(&other)?);
        Ok(())
    }
}
