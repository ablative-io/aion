use std::sync::Arc;

use aion_core::{
    ActivityId, ContentType, Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus,
};
use aion_package::ContentHash;
use aion_store::{EventStore, WriteToken};
use serde_json::json;

use super::{
    ActivityAwaitStep, RetryLoopTerminal, RetryRecorderSeam, await_activity_step, correlation_id,
    dispatch_with_retries, spawn_completion_task,
};
use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use crate::durability::Recorder;
use crate::error::EngineError;
use crate::registry::{
    CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
};
use crate::runtime::nif_state::EngineNifState;
use crate::runtime::nif_test_stores::StaleReadStore;
use crate::runtime::nif_timeout::TimeoutScope;
use crate::runtime::{RuntimeConfig, RuntimeHandle, SignalDeliveryConfig};

type TestResult = Result<(), Box<dyn std::error::Error>>;

include!("await_support.rs");
include!("blocking_support.rs");
include!("retry_support.rs");

mod await_paths;
mod retry_outcomes;
mod retry_parking;
