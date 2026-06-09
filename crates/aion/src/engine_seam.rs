//! Engine-facing seam for time, signal, query, child, and concurrency services.
//!
//! AE implements [`EngineHandle`] for the real engine. This AT cluster consumes the seam to resolve
//! workflow residency, deliver already-recorded observations to mailboxes, request linked child
//! workflow starts, arm timer-wheel entries, and route asynchronous-arrival events through the
//! target workflow's single AD Recorder. AT does not manage workflow process lifecycle,
//! supervision, or module loading directly.

use aion_core::{Event, Payload, RunId, TimerId, WorkflowError, WorkflowId};

use crate::Pid;
use chrono::{DateTime, Utc};
use tokio::sync::oneshot;

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

/// One-shot reply path carried by read-only query mailbox messages.
///
/// Workflow processes answer query messages at yield points from registered read-only handlers.
/// Queries are distinct from signals, do not mutate deterministic workflow state, and never record
/// events; the reply sender carries either the handler payload or a typed query error.
pub type QueryReplySender = oneshot::Sender<crate::query::service::QueryResult>;

/// Message kinds AT may ask AE to deliver to a workflow process mailbox.
#[derive(Debug)]
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
        /// One-shot channel for the workflow query handler's reply.
        reply_to: QueryReplySender,
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

/// Parent/child process relationship requested for a child workflow spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildWorkflowSpawnMode {
    /// Link the child to the parent so parent cancellation propagates as an exit signal.
    Linked,
    /// Detach the child from blocking await semantics and monitor it for terminal exits.
    DetachedMonitor,
}

/// Request from AT to AE to spawn a child workflow under a parent process.
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
    /// Relationship AE should establish between parent and child processes.
    pub mode: ChildWorkflowSpawnMode,
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

    /// AE could not terminate a linked child process.
    #[error("linked child termination failed: {reason}")]
    ChildTermination {
        /// Human-readable child termination failure reason.
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

    /// Terminates a linked child workflow process through AE's process-link boundary.
    ///
    /// # Errors
    ///
    /// Returns [`EngineSeamError`] when AE cannot send the cancellation exit to the linked child.
    fn terminate_linked_child_workflow(
        &self,
        parent_workflow_id: &WorkflowId,
        child_process: WorkflowProcessHandle,
        correlation: u64,
    ) -> Result<(), EngineSeamError>;

    /// Terminates a linked in-VM activity process through AE's process-link boundary.
    ///
    /// # Errors
    ///
    /// Returns [`EngineSeamError`] when AE cannot send the cancellation exit to the linked child.
    fn terminate_linked_activity(
        &self,
        parent_workflow_id: &WorkflowId,
        activity_process: Pid,
        correlation: u64,
    ) -> Result<(), EngineSeamError>;

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

    use aion_store::{WritableEventStore, WriteToken};

    use super::*;

    /// Operation captured by [`FakeEngineHandle`] in observed order.
    #[derive(Clone, Debug, PartialEq)]
    pub enum FakeEngineOperation {
        /// A mailbox message was delivered.
        Delivered {
            /// Target process handle.
            process: WorkflowProcessHandle,
            /// Delivered message projection.
            message: DeliveredWorkflowMessage,
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
        /// A linked child workflow process was terminated.
        LinkedChildWorkflowTerminated {
            /// Parent workflow owning the link.
            parent_workflow_id: WorkflowId,
            /// Linked child workflow process.
            child_process: WorkflowProcessHandle,
            /// Spawn correlation token.
            correlation: u64,
        },
        /// A linked activity process was terminated.
        LinkedActivityTerminated {
            /// Parent workflow owning the link.
            parent_workflow_id: WorkflowId,
            /// Linked activity process.
            activity_process: Pid,
            /// Spawn correlation token.
            correlation: u64,
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
        delivered: Vec<(WorkflowProcessHandle, DeliveredWorkflowMessage)>,
        delivery_responses: VecDeque<Result<(), EngineSeamError>>,
        child_spawn_requests: Vec<ChildWorkflowSpawnRequest>,
        child_spawn_responses: VecDeque<Result<ChildWorkflowSpawnResult, EngineSeamError>>,
        armed_timers: Vec<TimerWheelEntry>,
        disarmed_timers: Vec<(WorkflowProcessHandle, TimerId)>,
        terminated_child_workflows: Vec<(WorkflowId, WorkflowProcessHandle, u64)>,
        terminated_activities: Vec<(WorkflowId, Pid, u64)>,
        recorded_events: Vec<(WorkflowId, Event)>,
        operations: Vec<FakeEngineOperation>,
        recorder_store: Option<Arc<dyn WritableEventStore>>,
        record_responses: VecDeque<Result<(), EngineSeamError>>,
        linked_children: HashMap<WorkflowId, Vec<WorkflowId>>,
        propagated_child_exits: Vec<(WorkflowId, WorkflowId)>,
    }

    /// Cloneable projection of delivered mailbox messages for seam tests.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum DeliveredWorkflowMessage {
        /// A timer-fired delivery was observed.
        TimerFired {
            timer_id: TimerId,
            fire_at: DateTime<Utc>,
        },
        /// A signal delivery was observed.
        SignalReceived { name: String, payload: Payload },
        /// A query delivery was observed; the one-shot sender is intentionally not retained.
        Query { name: String, payload: Payload },
        /// A child completion delivery was observed.
        ChildWorkflowCompleted {
            child_workflow_id: WorkflowId,
            correlation: u64,
            result: Payload,
        },
        /// A child failure delivery was observed.
        ChildWorkflowFailed {
            child_workflow_id: WorkflowId,
            correlation: u64,
            error: WorkflowError,
        },
        /// A child cancellation delivery was observed.
        ChildWorkflowCancelled {
            child_workflow_id: WorkflowId,
            correlation: u64,
        },
    }

    impl DeliveredWorkflowMessage {
        pub(crate) fn from_message(message: &WorkflowMailboxMessage) -> Self {
            match message {
                WorkflowMailboxMessage::TimerFired { timer_id, fire_at } => Self::TimerFired {
                    timer_id: timer_id.clone(),
                    fire_at: *fire_at,
                },
                WorkflowMailboxMessage::SignalReceived { name, payload } => Self::SignalReceived {
                    name: name.clone(),
                    payload: payload.clone(),
                },
                WorkflowMailboxMessage::Query {
                    name,
                    payload,
                    reply_to: _,
                } => Self::Query {
                    name: name.clone(),
                    payload: payload.clone(),
                },
                WorkflowMailboxMessage::ChildWorkflowCompleted {
                    child_workflow_id,
                    correlation,
                    result,
                } => Self::ChildWorkflowCompleted {
                    child_workflow_id: child_workflow_id.clone(),
                    correlation: *correlation,
                    result: result.clone(),
                },
                WorkflowMailboxMessage::ChildWorkflowFailed {
                    child_workflow_id,
                    correlation,
                    error,
                } => Self::ChildWorkflowFailed {
                    child_workflow_id: child_workflow_id.clone(),
                    correlation: *correlation,
                    error: error.clone(),
                },
                WorkflowMailboxMessage::ChildWorkflowCancelled {
                    child_workflow_id,
                    correlation,
                } => Self::ChildWorkflowCancelled {
                    child_workflow_id: child_workflow_id.clone(),
                    correlation: *correlation,
                },
            }
        }
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
        pub fn recording_to(store: Arc<dyn WritableEventStore>) -> Self {
            Self {
                state: Mutex::new(FakeEngineState {
                    recorder_store: Some(store),
                    ..FakeEngineState::default()
                }),
            }
        }

        /// Sets the residency response returned for a workflow.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn set_residency(
            &self,
            workflow_id: WorkflowId,
            residency: WorkflowResidency,
        ) -> Result<(), EngineSeamError> {
            self.state()?.residency.insert(workflow_id, residency);
            Ok(())
        }

        /// Queues the next response returned by mailbox-delivery seam calls.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn push_delivery_response(
            &self,
            response: Result<(), EngineSeamError>,
        ) -> Result<(), EngineSeamError> {
            self.state()?.delivery_responses.push_back(response);
            Ok(())
        }

        /// Returns a snapshot of seam operations in observed order.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn operations(&self) -> Result<Vec<FakeEngineOperation>, EngineSeamError> {
            Ok(self.state()?.operations.clone())
        }

        /// Returns a snapshot of delivered mailbox messages.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn delivered_messages(
            &self,
        ) -> Result<Vec<(WorkflowProcessHandle, DeliveredWorkflowMessage)>, EngineSeamError>
        {
            Ok(self.state()?.delivered.clone())
        }

        /// Returns a snapshot of armed timer-wheel entries.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn armed_timers(&self) -> Result<Vec<TimerWheelEntry>, EngineSeamError> {
            Ok(self.state()?.armed_timers.clone())
        }

        /// Queues the next child-spawn response returned by the fake.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn push_child_spawn_response(
            &self,
            response: Result<ChildWorkflowSpawnResult, EngineSeamError>,
        ) -> Result<(), EngineSeamError> {
            self.state()?.child_spawn_responses.push_back(response);
            Ok(())
        }

        /// Returns captured child-spawn requests in observed order.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn child_spawn_requests(
            &self,
        ) -> Result<Vec<ChildWorkflowSpawnRequest>, EngineSeamError> {
            Ok(self.state()?.child_spawn_requests.clone())
        }

        /// Returns events recorded through the fake recorder seam.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn recorded_events(&self) -> Result<Vec<(WorkflowId, Event)>, EngineSeamError> {
            Ok(self.state()?.recorded_events.clone())
        }

        /// Returns linked child workflow termination calls observed by the fake.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn terminated_child_workflows(
            &self,
        ) -> Result<Vec<(WorkflowId, WorkflowProcessHandle, u64)>, EngineSeamError> {
            Ok(self.state()?.terminated_child_workflows.clone())
        }

        /// Returns linked activity termination calls observed by the fake.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn terminated_activities(
            &self,
        ) -> Result<Vec<(WorkflowId, Pid, u64)>, EngineSeamError> {
            Ok(self.state()?.terminated_activities.clone())
        }

        /// Simulates AE terminating a parent process and propagating exits to linked children.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn terminate_parent(&self, parent: &WorkflowId) -> Result<(), EngineSeamError> {
            let mut state = self.state()?;
            if let Some(children) = state.linked_children.get(parent).cloned() {
                for child in children {
                    state.propagated_child_exits.push((parent.clone(), child));
                }
            }
            Ok(())
        }

        /// Returns propagated linked-child exits observed by the fake.
        ///
        /// # Errors
        ///
        /// Returns [`EngineSeamError::EngineOffline`] if the fake's state lock is poisoned.
        pub fn propagated_child_exits(
            &self,
        ) -> Result<Vec<(WorkflowId, WorkflowId)>, EngineSeamError> {
            Ok(self.state()?.propagated_child_exits.clone())
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
            if let Some(response) = state.delivery_responses.pop_front() {
                response?;
            }
            let delivered = DeliveredWorkflowMessage::from_message(&message);
            state.delivered.push((process, delivered.clone()));
            state.operations.push(FakeEngineOperation::Delivered {
                process,
                message: delivered,
            });
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
                .push(FakeEngineOperation::ChildSpawnRequested(request.clone()));
            if let Some(response) = state.child_spawn_responses.pop_front() {
                if let Ok(result) = &response {
                    if request.mode == ChildWorkflowSpawnMode::Linked {
                        state
                            .linked_children
                            .entry(request.parent_workflow_id.clone())
                            .or_default()
                            .push(result.child_workflow_id.clone());
                    }
                }
                response
            } else {
                Err(EngineSeamError::ChildSpawn {
                    reason: "fake child spawn response was not queued".to_owned(),
                })
            }
        }

        fn terminate_linked_child_workflow(
            &self,
            parent_workflow_id: &WorkflowId,
            child_process: WorkflowProcessHandle,
            correlation: u64,
        ) -> Result<(), EngineSeamError> {
            let mut state = self.state()?;
            state.terminated_child_workflows.push((
                parent_workflow_id.clone(),
                child_process,
                correlation,
            ));
            state
                .operations
                .push(FakeEngineOperation::LinkedChildWorkflowTerminated {
                    parent_workflow_id: parent_workflow_id.clone(),
                    child_process,
                    correlation,
                });
            Ok(())
        }

        fn terminate_linked_activity(
            &self,
            parent_workflow_id: &WorkflowId,
            activity_process: Pid,
            correlation: u64,
        ) -> Result<(), EngineSeamError> {
            let mut state = self.state()?;
            state.terminated_activities.push((
                parent_workflow_id.clone(),
                activity_process,
                correlation,
            ));
            state
                .operations
                .push(FakeEngineOperation::LinkedActivityTerminated {
                    parent_workflow_id: parent_workflow_id.clone(),
                    activity_process,
                    correlation,
                });
            Ok(())
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
            if let Some(response) = state.record_responses.pop_front() {
                response?;
            }
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
                futures::executor::block_on(store.append(
                    WriteToken::recorder(),
                    workflow_id,
                    &[event],
                    expected_seq,
                ))
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

    use super::test_support::{DeliveredWorkflowMessage, FakeEngineHandle};
    use super::{
        EngineHandle, EngineSeamError, WorkflowMailboxMessage, WorkflowProcessHandle,
        WorkflowResidency,
    };

    #[test]
    fn fake_captures_delivered_message_for_resident_workflow()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let workflow_id = WorkflowId::new_v4();
        let process = WorkflowProcessHandle::new(42);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        let resolved = engine.resolve_workflow(&workflow_id)?;
        assert_eq!(resolved, WorkflowResidency::Resident(process));

        let payload = Payload::new(ContentType::Json, b"null".to_vec());
        let message = WorkflowMailboxMessage::SignalReceived {
            name: "wake".to_owned(),
            payload: payload.clone(),
        };
        engine.deliver_workflow_message(process, message)?;

        assert_eq!(
            engine.delivered_messages()?,
            vec![(
                process,
                DeliveredWorkflowMessage::SignalReceived {
                    name: "wake".to_owned(),
                    payload,
                }
            )]
        );
        Ok(())
    }

    #[test]
    fn fake_can_inject_delivery_failure() -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let process = WorkflowProcessHandle::new(43);
        engine.push_delivery_response(Err(EngineSeamError::Delivery {
            reason: "mailbox unavailable".to_owned(),
        }))?;

        let error = engine
            .deliver_workflow_message(
                process,
                WorkflowMailboxMessage::SignalReceived {
                    name: "wake".to_owned(),
                    payload: Payload::new(ContentType::Json, b"null".to_vec()),
                },
            )
            .err()
            .ok_or_else(|| std::io::Error::other("delivery failure was not returned"))?;

        assert!(matches!(error, EngineSeamError::Delivery { .. }));
        assert!(engine.delivered_messages()?.is_empty());
        assert!(engine.operations()?.is_empty());
        Ok(())
    }
}
