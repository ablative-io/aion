use std::sync::Arc;

use aion_core::{
    ActivityError, ActivityErrorKind, ActivityId, ContentType, Event, EventEnvelope, Payload,
    RunId, WorkflowId, WorkflowStatus,
};
use aion_package::ContentHash;
use aion_store::{EventStore, InMemoryStore, WriteToken};
use serde_json::json;

use super::{
    ActivitySpec, CollectDeps, CollectStep, collect_step, fan_out_items, fan_out_items_recovered,
    scheduled_node, scheduled_task_queue,
};
use crate::activity::bridge::ActivityDispatch;
use crate::durability::Recorder;
use crate::registry::{
    CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
};
use crate::runtime::nif_state::{CollectKind, EngineNifState, PendingAwait};
use crate::runtime::nif_timeout::TimeoutScope;
use crate::runtime::{RuntimeConfig, RuntimeHandle};

type TestResult = Result<(), Box<dyn std::error::Error>>;

include!("harness.rs");
include!("events.rs");
include!("fixtures.rs");

mod expiry;
mod routing_fresh;
mod routing_recovery;
mod settlement;
