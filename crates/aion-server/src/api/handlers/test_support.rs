//! Shared fixtures and request builders for the handler tests.

use std::sync::Arc;

use aion::{Engine, EngineBuilder};
use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowId};
use aion_proto::{
    ProtoCancelRequest, ProtoDescribeWorkflowRequest, ProtoQueryRequest, ProtoSignalRequest,
    WireError, WireErrorCode, convert::ProtoPayload,
};
use aion_store::{EventStore, InMemoryStore, WriteToken, visibility::VisibilityStore};
use chrono::Utc;
use serde_json::json;

use crate::{
    CallerIdentity, NamespaceGuard, NamespaceResolver, StaticScheduleNamespaces,
    StaticWorkflowNamespaces, config::NamespaceMode,
};

pub(super) const NAMESPACE: &str = "tenant-a";

pub(super) struct TestContext {
    pub(super) guard: NamespaceGuard,
    pub(super) caller: CallerIdentity,
    pub(super) ownership: StaticWorkflowNamespaces,
    pub(super) store: Arc<dyn EventStore>,
    pub(super) visibility_store: Arc<dyn VisibilityStore>,
}

pub(super) async fn context() -> Result<TestContext, aion::EngineError> {
    let backing = Arc::new(InMemoryStore::default());
    let store: Arc<dyn EventStore> = backing.clone();
    let visibility_store: Arc<dyn VisibilityStore> = backing;
    let engine = Arc::new(
        EngineBuilder::new()
            .store_arc(Arc::clone(&store))
            .visibility_store_arc(Arc::clone(&visibility_store))
            .scheduler_threads(1)
            .build()
            .await?,
    );
    Ok(context_from_engine(engine, store, visibility_store))
}

fn context_from_engine(
    engine: Arc<Engine>,
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
) -> TestContext {
    let ownership = StaticWorkflowNamespaces::default();
    let resolver = NamespaceResolver::from_parts(
        NamespaceMode::SharedEngine,
        Some(engine),
        Arc::new(ownership.clone()),
        Arc::new(StaticScheduleNamespaces::default()),
    );
    TestContext {
        guard: NamespaceGuard::new(resolver),
        caller: CallerIdentity::new("alice", [NAMESPACE.to_owned()]),
        ownership,
        store,
        visibility_store,
    }
}

pub(super) fn denied_guard() -> (NamespaceGuard, CallerIdentity) {
    let ownership = StaticWorkflowNamespaces::default();
    let resolver = NamespaceResolver::authorization_only(
        NamespaceMode::SharedEngine,
        ownership,
        StaticScheduleNamespaces::default(),
    );
    let guard = NamespaceGuard::new(resolver);
    let caller = CallerIdentity::new("alice", [String::from("tenant-b")]);
    (guard, caller)
}

pub(super) fn assert_workflow_not_found<T>(result: Result<T, WireError>) -> Result<(), WireError> {
    let error = result
        .err()
        .ok_or_else(|| WireError::backend("expected error"))?;
    assert_eq!(error.code, WireErrorCode::NotFound);
    assert_eq!(error.error_type.as_deref(), Some("WorkflowNotFound"));
    assert_eq!(
        error.message,
        format!("workflow {} not found", workflow_id())
    );
    Ok(())
}

pub(super) async fn append_started(
    store: &dyn EventStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let event = started_event()?;
    store
        .append(WriteToken::recorder(), &workflow_id(), &[event], 0)
        .await?;
    Ok(())
}

pub(super) async fn append_completed(
    store: &dyn EventStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let events = [
        started_event()?,
        Event::WorkflowCompleted {
            envelope: event_envelope(2),
            result: payload()?,
        },
    ];
    store
        .append(WriteToken::recorder(), &workflow_id(), &events, 0)
        .await?;
    Ok(())
}

pub(super) async fn append_failed(
    store: &dyn EventStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let events = [
        started_event()?,
        Event::WorkflowFailed {
            envelope: event_envelope(2),
            error: aion_core::WorkflowError {
                message: "fixture failure".to_owned(),
                details: None,
            },
        },
    ];
    store
        .append(WriteToken::recorder(), &workflow_id(), &events, 0)
        .await?;
    Ok(())
}

pub(super) async fn append_continued_chain(
    store: &dyn EventStore,
    first: &RunId,
    latest: &RunId,
) -> Result<(), Box<dyn std::error::Error>> {
    let events = [
        Event::WorkflowStarted {
            envelope: event_envelope(1),
            workflow_type: "fixture".to_owned(),
            input: payload()?,
            run_id: first.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
        Event::WorkflowContinuedAsNew {
            envelope: event_envelope(2),
            input: payload()?,
            workflow_type: None,
            parent_run_id: first.clone(),
        },
        Event::WorkflowStarted {
            envelope: event_envelope(3),
            workflow_type: "fixture".to_owned(),
            input: payload()?,
            run_id: latest.clone(),
            parent_run_id: Some(first.clone()),
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
    ];
    store
        .append(WriteToken::recorder(), &workflow_id(), &events, 0)
        .await?;
    Ok(())
}

pub(super) fn signal_request() -> Result<ProtoSignalRequest, aion_core::PayloadError> {
    Ok(ProtoSignalRequest {
        namespace: NAMESPACE.to_owned(),
        workflow_id: Some(workflow_id().into()),
        run_id: Some(run_id().into()),
        signal_name: "poke".to_owned(),
        payload: Some(proto_payload()?),
    })
}

pub(super) fn query_request() -> ProtoQueryRequest {
    ProtoQueryRequest {
        namespace: NAMESPACE.to_owned(),
        workflow_id: Some(workflow_id().into()),
        run_id: Some(run_id().into()),
        query_name: "state".to_owned(),
    }
}

pub(super) fn cancel_request() -> ProtoCancelRequest {
    ProtoCancelRequest {
        namespace: NAMESPACE.to_owned(),
        workflow_id: Some(workflow_id().into()),
        run_id: Some(run_id().into()),
        reason: "test cancellation".to_owned(),
    }
}

pub(super) fn describe_request(
    include_history: bool,
    run_id: Option<RunId>,
) -> ProtoDescribeWorkflowRequest {
    ProtoDescribeWorkflowRequest {
        namespace: NAMESPACE.to_owned(),
        workflow_id: Some(workflow_id().into()),
        run_id: run_id.map(Into::into),
        include_history,
    }
}

fn started_event() -> Result<Event, aion_core::PayloadError> {
    Ok(Event::WorkflowStarted {
        envelope: event_envelope(1),
        workflow_type: "fixture".to_owned(),
        input: payload()?,
        run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    })
}

fn event_envelope(seq: u64) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: Utc::now(),
        workflow_id: workflow_id(),
    }
}

pub(super) fn proto_payload() -> Result<ProtoPayload, aion_core::PayloadError> {
    Ok(payload()?.into())
}

fn payload() -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "fixture": "input" }))
}

pub(super) fn workflow_id() -> WorkflowId {
    WorkflowId::new(uuid::Uuid::from_u128(1))
}

pub(super) fn run_id() -> RunId {
    RunId::new(uuid::Uuid::from_u128(2))
}
