//! Engine-facing seam for time, signal, query, child, and concurrency services.
//!
//! AE implements [`EngineHandle`] for the real engine. This AT cluster consumes the seam to resolve
//! workflow residency, deliver already-recorded observations to mailboxes, request linked child
//! workflow starts, arm timer-wheel entries, and route asynchronous-arrival events through the
//! target workflow's single AD Recorder. AT does not manage workflow process lifecycle,
//! supervision, or module loading directly.

use aion_core::{Event, Payload, RunId, TimerId, WorkflowError, WorkflowId};
use chrono::{DateTime, Utc};

/// Narrow live-process handle used by AT services after AE resolves workflow residency.
///
/// The wrapper intentionally exposes only an opaque process identifier. Real AE implementations can
/// adapt their concrete BEAM process handle into this type without giving AT lifecycle ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkflowProcessHandle {
    pid: u64,
}

impl WorkflowProcessHandle {
    /// Creates a workflow process handle from an opaque process identifier.
    #[must_use]
    pub const fn new(pid: u64) -> Self {
        Self { pid }
    }

    /// Returns the opaque process identifier backing this handle.
    #[must_use]
    pub const fn pid(self) -> u64 {
        self.pid
    }
}

/// AE's answer when AT resolves a logical workflow to a live process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowResidency {
    /// The workflow is currently resident and can receive mailbox messages.
    Resident(WorkflowProcessHandle),
    /// The workflow exists durably but has no live process at the moment.
    NonResident,
    /// The workflow is terminal and should not receive live interactions.
    Terminal,
    /// AE has no durable or live workflow for the requested identifier.
    Unknown,
}

/// Message kinds AT may ask AE to deliver to a workflow process mailbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowMailboxMessage {
    /// A durable timer fired and has been recorded.
    TimerFired {
        /// Timer that fired.
        timer_id: TimerId,
        /// Deterministic fire timestamp carried for service/replay correlation.
        fire_at: DateTime<Utc>,
    },
    /// A durable signal arrived and has been recorded.
    SignalReceived {
        /// Signal name selected by the sender.
        name: String,
        /// Opaque signal payload.
        payload: Payload,
    },
    /// A read-only query request. Query dispatch records no event.
    Query {
        /// Query name selected by the caller.
        name: String,
        /// Opaque query input payload.
        payload: Payload,
        /// Engine-assigned correlation token for the reply path.
        correlation: u64,
    },
    /// A linked child workflow completed successfully and has been recorded.
    ChildWorkflowCompleted {
        /// Child workflow that produced the result.
        child_workflow_id: WorkflowId,
        /// Spawn correlation token used by collectors.
        correlation: u64,
        /// Opaque child result payload.
        result: Payload,
    },
    /// A linked child workflow failed terminally and has been recorded.
    ChildWorkflowFailed {
        /// Child workflow that failed.
        child_workflow_id: WorkflowId,
        /// Spawn correlation token used by collectors.
        correlation: u64,
        /// Terminal child workflow failure.
        error: WorkflowError,
    },
    /// A linked child workflow was cancelled and has been recorded.
    ChildWorkflowCancelled {
        /// Child workflow that was cancelled.
        child_workflow_id: WorkflowId,
        /// Spawn correlation token used by collectors.
        correlation: u64,
    },
}

/// Request from AT to AE to spawn a child workflow linked to a parent process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildWorkflowSpawnRequest {
    /// Parent workflow whose process owns the link.
    pub parent_workflow_id: WorkflowId,
    /// Child workflow type selected by the parent workflow.
    pub workflow_type: String,
    /// Opaque child workflow input payload.
    pub input: Payload,
    /// Concrete run identifier requested for the child execution.
    pub run_id: RunId,
}

/// AE's result after starting a linked child workflow execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildWorkflowSpawnResult {
    /// Logical child workflow identifier.
    pub child_workflow_id: WorkflowId,
    /// Live process handle for the linked child execution.
    pub child_process: WorkflowProcessHandle,
}

/// Timer-wheel entry requested by AT for a live workflow process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimerWheelEntry {
    /// Workflow process that should receive the timer fire path.
    pub process: WorkflowProcessHandle,
    /// Timer selected by workflow code or assigned by the engine.
    pub timer_id: TimerId,
    /// UTC timestamp at which the wheel should fire.
    pub fire_at: DateTime<Utc>,
}

/// Errors returned by the engine seam.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum EngineSeamError {
    /// The target workflow has no current live process.
    #[error("workflow {workflow_id} is not resident")]
    NonResident {
        /// Workflow that had no current live process.
        workflow_id: WorkflowId,
    },

    /// The target workflow is terminal.
    #[error("workflow {workflow_id} is terminal")]
    Terminal {
        /// Terminal workflow identifier.
        workflow_id: WorkflowId,
    },

    /// The target workflow is unknown to AE.
    #[error("workflow {workflow_id} is unknown")]
    UnknownWorkflow {
        /// Unknown workflow identifier.
        workflow_id: WorkflowId,
    },

    /// AE could not deliver a mailbox message.
    #[error("mailbox delivery failed: {reason}")]
    Delivery {
        /// Human-readable delivery failure reason.
        reason: String,
    },

    /// AE could not spawn a linked child workflow.
    #[error("child workflow spawn failed: {reason}")]
    ChildSpawn {
        /// Human-readable child-spawn failure reason.
        reason: String,
    },

    /// AE could not arm or disarm the timer wheel.
    #[error("timer wheel operation failed: {reason}")]
    TimerWheel {
        /// Human-readable timer-wheel failure reason.
        reason: String,
    },

    /// AD's single Recorder path could not record the event.
    #[error("workflow recorder failed: {reason}")]
    Recorder {
        /// Human-readable recorder failure reason.
        reason: String,
    },
}

/// Engine-facing capabilities consumed by AT services and implemented by AE.
///
/// This trait deliberately does not expose operations that start, supervise, tear down, or load
/// top-level workflow processes. Child spawning, residency resolution, and recording are requests
/// into AE/AD-owned infrastructure. In particular, [`EngineHandle::record_workflow_event`] must
/// route asynchronous-arrival events through the target workflow's single Recorder; AT services must
/// not append directly to the event store.
pub trait EngineHandle: Send + Sync {
    /// Resolves a workflow identifier to its current residency state.
    ///
    /// # Errors
    ///
    /// Returns [`EngineSeamError`] when AE cannot inspect residency for the requested workflow.
    fn resolve_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowResidency, EngineSeamError>;

    /// Delivers a message to a resident workflow process mailbox.
    ///
    /// # Errors
    ///
    /// Returns [`EngineSeamError`] when AE cannot enqueue the message on the target mailbox.
    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError>;

    /// Requests AE to spawn a child workflow execution linked to the parent process.
    ///
    /// # Errors
    ///
    /// Returns [`EngineSeamError`] when AE rejects or fails the linked child-spawn request.
    fn spawn_child_workflow(
        &self,
        request: ChildWorkflowSpawnRequest,
    ) -> Result<ChildWorkflowSpawnResult, EngineSeamError>;

    /// Arms a timer-wheel entry for a resident workflow process.
    ///
    /// # Errors
    ///
    /// Returns [`EngineSeamError`] when AE cannot register the timer with the live wheel.
    fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError>;

    /// Disarms a timer-wheel entry for a resident workflow process.
    ///
    /// # Errors
    ///
    /// Returns [`EngineSeamError`] when AE cannot remove the timer from the live wheel.
    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &TimerId,
    ) -> Result<(), EngineSeamError>;

    /// Records an event through the target workflow's single AD Recorder.
    ///
    /// # Errors
    ///
    /// Returns [`EngineSeamError`] when the target workflow's Recorder cannot append the event.
    fn record_workflow_event(
        &self,
        workflow_id: &WorkflowId,
        event: Event,
    ) -> Result<(), EngineSeamError>;
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;
    use std::sync::{Mutex, MutexGuard};

    use aion_store::EventStore;

    use super::*;

    /// Operation captured by [`FakeEngineHandle`] in observed order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FakeEngineOperation {
        /// A mailbox message was delivered.
        Delivered {
            /// Target process handle.
            process: WorkflowProcessHandle,
            /// Delivered message.
            message: WorkflowMailboxMessage,
        },
        /// A child spawn was requested.
        ChildSpawnRequested(ChildWorkflowSpawnRequest),
        /// A timer-wheel entry was armed.
        TimerArmed(TimerWheelEntry),
        /// A timer-wheel entry was disarmed.
        TimerDisarmed {
            /// Target process handle.
            process: WorkflowProcessHandle,
            /// Timer that was disarmed.
            timer_id: TimerId,
        },
        /// An event was recorded through the recorder seam.
        EventRecorded {
            /// Workflow whose recorder received the event.
            workflow_id: WorkflowId,
            /// Recorded event.
            event: Event,
        },
    }

    #[derive(Default)]
    struct FakeEngineState {
        residency: HashMap<WorkflowId, WorkflowResidency>,
        delivered: Vec<(WorkflowProcessHandle, WorkflowMailboxMessage)>,
        child_spawn_requests: Vec<ChildWorkflowSpawnRequest>,
        child_spawn_responses: VecDeque<Result<ChildWorkflowSpawnResult, EngineSeamError>>,
        armed_timers: Vec<TimerWheelEntry>,
        disarmed_timers: Vec<(WorkflowProcessHandle, TimerId)>,
        recorded_events: Vec<(WorkflowId, Event)>,
        operations: Vec<FakeEngineOperation>,
        recorder_store: Option<Arc<dyn EventStore>>,
    }

    /// Test-only fake implementation of [`EngineHandle`].
    #[derive(Default)]
    pub struct FakeEngineHandle {
        state: Mutex<FakeEngineState>,
    }

    impl FakeEngineHandle {
        /// Creates an empty fake engine handle.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Creates a fake whose recorder seam appends to the supplied store with event sequencing.
        #[must_use]
        pub fn recording_to(store: Arc<dyn EventStore>) -> Self {
            Self {
                state: Mutex::new(FakeEngineState {
                    recorder_store: Some(store),
                    ..FakeEngineState::default()
                }),
            }
        }

        /// Sets the residency response returned for a workflow.
        pub fn set_residency(
            &self,
            workflow_id: WorkflowId,
            residency: WorkflowResidency,
        ) -> Result<(), EngineSeamError> {
            self.state()?.residency.insert(workflow_id, residency);
            Ok(())
        }

        /// Returns a snapshot of delivered mailbox messages.
        pub fn delivered_messages(
            &self,
        ) -> Result<Vec<(WorkflowProcessHandle, WorkflowMailboxMessage)>, EngineSeamError> {
            Ok(self.state()?.delivered.clone())
        }

        /// Returns a snapshot of armed timer-wheel entries.
        pub fn armed_timers(&self) -> Result<Vec<TimerWheelEntry>, EngineSeamError> {
            Ok(self.state()?.armed_timers.clone())
        }

        /// Returns a snapshot of recorded events.
        pub fn recorded_events(&self) -> Result<Vec<(WorkflowId, Event)>, EngineSeamError> {
            Ok(self.state()?.recorded_events.clone())
        }

        fn state(&self) -> Result<MutexGuard<'_, FakeEngineState>, EngineSeamError> {
            self.state.lock().map_err(|_| EngineSeamError::Recorder {
                reason: "fake engine state lock was poisoned".to_owned(),
            })
        }
    }

    impl EngineHandle for FakeEngineHandle {
        fn resolve_workflow(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<WorkflowResidency, EngineSeamError> {
            Ok(self
                .state()?
                .residency
                .get(workflow_id)
                .copied()
                .unwrap_or(WorkflowResidency::Unknown))
        }

        fn deliver_workflow_message(
            &self,
            process: WorkflowProcessHandle,
            message: WorkflowMailboxMessage,
        ) -> Result<(), EngineSeamError> {
            let mut state = self.state()?;
            state.delivered.push((process, message.clone()));
            state
                .operations
                .push(FakeEngineOperation::Delivered { process, message });
            Ok(())
        }

        fn spawn_child_workflow(
            &self,
            request: ChildWorkflowSpawnRequest,
        ) -> Result<ChildWorkflowSpawnResult, EngineSeamError> {
            let mut state = self.state()?;
            state.child_spawn_requests.push(request.clone());
            state
                .operations
                .push(FakeEngineOperation::ChildSpawnRequested(request));
            if let Some(response) = state.child_spawn_responses.pop_front() {
                response
            } else {
                Err(EngineSeamError::ChildSpawn {
                    reason: "fake child spawn response was not queued".to_owned(),
                })
            }
        }

        fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError> {
            let mut state = self.state()?;
            state.armed_timers.push(entry.clone());
            state
                .operations
                .push(FakeEngineOperation::TimerArmed(entry));
            Ok(())
        }

        fn disarm_timer(
            &self,
            process: WorkflowProcessHandle,
            timer_id: &TimerId,
        ) -> Result<(), EngineSeamError> {
            let mut state = self.state()?;
            state
                .armed_timers
                .retain(|entry| !(entry.process == process && &entry.timer_id == timer_id));
            state.disarmed_timers.push((process, timer_id.clone()));
            state.operations.push(FakeEngineOperation::TimerDisarmed {
                process,
                timer_id: timer_id.clone(),
            });
            Ok(())
        }

        fn record_workflow_event(
            &self,
            workflow_id: &WorkflowId,
            event: Event,
        ) -> Result<(), EngineSeamError> {
            let mut state = self.state()?;
            state
                .recorded_events
                .push((workflow_id.clone(), event.clone()));
            let recorder_store = state.recorder_store.clone();
            state.operations.push(FakeEngineOperation::EventRecorded {
                workflow_id: workflow_id.clone(),
                event: event.clone(),
            });
            drop(state);

            if let Some(store) = recorder_store {
                let expected_seq = event.seq().saturating_sub(1);
                futures::executor::block_on(store.append(workflow_id, &[event], expected_seq))
                    .map_err(|error| EngineSeamError::Recorder {
                        reason: error.to_string(),
                    })?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{ContentType, Payload, WorkflowId};

    use super::test_support::FakeEngineHandle;
    use super::{EngineHandle, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency};

    #[test]
    fn fake_captures_delivered_message_for_resident_workflow()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let workflow_id = WorkflowId::new_v4();
        let process = WorkflowProcessHandle::new(42);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        let resolved = engine.resolve_workflow(&workflow_id)?;
        assert_eq!(resolved, WorkflowResidency::Resident(process));

        let message = WorkflowMailboxMessage::SignalReceived {
            name: "wake".to_owned(),
            payload: Payload::new(ContentType::Json, b"null".to_vec()),
        };
        engine.deliver_workflow_message(process, message.clone())?;

        assert_eq!(engine.delivered_messages()?, vec![(process, message)]);
        Ok(())
    }
}
