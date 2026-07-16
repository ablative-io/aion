//! End-to-end workflow query coverage over the real server handler path.
//!
//! Every test drives `handlers::query` — the shared handler behind both the
//! HTTP route and the gRPC service — through the production namespace guard
//! against a real engine running the committed BEAM query fixture
//! (`crates/aion/tests/fixtures/aion_fixture_query.erl`, reused here exactly
//! as the namespace tests reuse `aion_fixture_workflow`; see
//! `tests/fixtures/README.md`). The matrix pins the #45 server contract:
//!
//! - happy path: `QueryResponse.outcome` carries the handler payload;
//! - query-semantic failures (`unknown_query`, `query_timeout`,
//!   `not_running`, `query_failed`) ride the `QueryResponse.error` oneof on a
//!   successful transport call;
//! - namespace failures stay transport-level and the cross-tenant anti-leak
//!   `NotFound` is byte-identical to probing a workflow that never existed.

use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{
    Payload, RunId, SearchAttributeSchema, SearchAttributeType, WorkflowId, WorkflowStatus,
};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_proto::{
    ProtoQueryRequest, ProtoQueryResponse, ProtoSignalRequest, ProtoStartWorkflowRequest,
    WireError, WireErrorCode, proto_query_response,
};
use aion_server::api::handlers;
use aion_server::config::{NamespaceConfig, NamespaceMode};
use aion_server::{CallerIdentity, NAMESPACE_ATTRIBUTE, NamespaceGuard, NamespaceResolver};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

type TestError = Box<dyn std::error::Error>;

const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";

/// Committed BEAM query fixture from the engine crate (hand-rolled pump loop
/// proving the raw sentinel protocol; handlers `state`, `boom`, plus the
/// pump-free `unpumped` entry for the timeout path).
const QUERY_MODULE: &str = "aion_fixture_query";
const QUERY_BEAM: &[u8] = include_bytes!("../../aion/tests/fixtures/aion_fixture_query.beam");
const QUERY_SOURCE: &[u8] = include_bytes!("../../aion/tests/fixtures/aion_fixture_query.erl");

/// Generous engine reply deadline for tests where queries must succeed.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for the fixture to finish registering its handlers (the
/// registration NIF runs asynchronously after `handlers::start` returns).
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(20);

/// One in-process "server": a real engine plus the production resolver/guard
/// wiring (`NamespaceResolver::from_config` installs the durable history
/// ownership sources, exactly as `ServerState` does), with the query seam
/// installed through `EngineBuilder::query_timeout` exactly as `state.rs`
/// installs it from the required `runtime.query_timeout_ms` config.
struct Server {
    engine: Arc<Engine>,
    guard: NamespaceGuard,
}

impl Server {
    async fn over(entry_function: &str, query_timeout: Duration) -> Result<Self, TestError> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let mut schema = SearchAttributeSchema::new();
        schema.register(NAMESPACE_ATTRIBUTE, SearchAttributeType::String)?;
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .search_attribute_schema(schema)
                .scheduler_threads(1)
                .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
                    Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
                })
                .query_timeout(query_timeout)
                .load_workflows(query_package(entry_function)?)
                .build()
                .await?,
        );
        let resolver = NamespaceResolver::from_config(
            NamespaceConfig {
                mode: NamespaceMode::SharedEngine,
            },
            Arc::clone(&engine),
        );
        Ok(Self {
            engine,
            guard: NamespaceGuard::new(resolver),
        })
    }

    fn shutdown(self) -> Result<(), TestError> {
        self.engine.shutdown()?;
        Ok(())
    }
}

fn query_package(entry_function: &str) -> Result<Package, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(QUERY_MODULE, QUERY_BEAM)])?;
    let manifest = Manifest {
        entry_module: QUERY_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(QUERY_MODULE, QUERY_SOURCE.to_vec())])
            .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

fn caller_for(subject: &str, namespace: &str) -> CallerIdentity {
    CallerIdentity::new(subject, [namespace.to_owned()])
}

fn ungranted_caller() -> CallerIdentity {
    CallerIdentity::new("mallory", Vec::<String>::new())
}

fn query_request(namespace: &str, workflow_id: &WorkflowId) -> ProtoQueryRequest {
    named_query_request(namespace, workflow_id, "state")
}

fn named_query_request(
    namespace: &str,
    workflow_id: &WorkflowId,
    query_name: &str,
) -> ProtoQueryRequest {
    ProtoQueryRequest {
        namespace: namespace.to_owned(),
        workflow_id: Some(workflow_id.clone().into()),
        run_id: None,
        query_name: query_name.to_owned(),
    }
}

/// Start the fixture through the real start handler (which stamps the
/// authorized namespace durably) without awaiting completion: the fixture
/// parks behind its pump until released.
async fn start_parked(
    server: &Server,
    caller: &CallerIdentity,
    namespace: &str,
) -> Result<(WorkflowId, RunId), TestError> {
    let response = handlers::start(
        &server.guard,
        caller,
        ProtoStartWorkflowRequest {
            namespace: namespace.to_owned(),
            workflow_type: QUERY_MODULE.to_owned(),
            input: Some(Payload::from_json(&json!({ "fixture": "input" }))?.into()),
            routing_key: None,
            task_queue: None,
        },
    )
    .await?;
    let workflow_id: WorkflowId = response
        .workflow_id
        .ok_or("start response missing workflow id")?
        .try_into()?;
    let run_id: RunId = response
        .run_id
        .ok_or("start response missing run id")?
        .try_into()?;
    Ok((workflow_id, run_id))
}

/// Decode a `QueryResponse` into either its result payload or its typed
/// outcome error.
fn decode_outcome(response: ProtoQueryResponse) -> Result<Result<Payload, WireError>, TestError> {
    match response.outcome {
        Some(proto_query_response::Outcome::Result(payload)) => Ok(Ok(payload.try_into()?)),
        Some(proto_query_response::Outcome::Error(error)) => Ok(Err(WireError::try_from(error)?)),
        None => Err("query response outcome is missing".into()),
    }
}

/// Query through the real handler, retrying while the fixture has not yet
/// executed its `register_query` calls (registration is workflow code, so it
/// races the caller after `handlers::start` returns). The first outcome that
/// is not an `unknown_query` outcome error — success or any other typed
/// outcome — is returned.
async fn query_when_registered(
    server: &Server,
    caller: &CallerIdentity,
    namespace: &str,
    workflow_id: &WorkflowId,
    query_name: &str,
) -> Result<Result<Payload, WireError>, TestError> {
    let deadline = std::time::Instant::now() + REGISTRATION_DEADLINE;
    loop {
        let response = handlers::query(
            &server.guard,
            caller,
            named_query_request(namespace, workflow_id, query_name),
        )
        .await?;
        match decode_outcome(response)? {
            Err(error)
                if error.code == WireErrorCode::UnknownQuery
                    && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            outcome => return Ok(outcome),
        }
    }
}

/// Decode the `state` handler's reply payload into `(answer, query_id)`.
fn state_reply(payload: &Payload) -> Result<(i64, String), TestError> {
    let value: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    let answer = value["answer"]
        .as_i64()
        .ok_or_else(|| format!("state reply missing answer: {value}"))?;
    let query_id = value["query_id"]
        .as_str()
        .ok_or_else(|| format!("state reply missing query_id: {value}"))?
        .to_owned();
    Ok((answer, query_id))
}

/// Release the parked fixture through the real signal handler and await its
/// known result so shutdown is clean.
async fn release_and_complete(
    server: &Server,
    caller: &CallerIdentity,
    namespace: &str,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<(), TestError> {
    handlers::signal(
        &server.guard,
        caller,
        ProtoSignalRequest {
            namespace: namespace.to_owned(),
            workflow_id: Some(workflow_id.clone().into()),
            run_id: Some(run_id.clone().into()),
            signal_name: "release".to_owned(),
            payload: Some(Payload::from_json(&json!({ "label": "release" }))?.into()),
        },
    )
    .await?;
    let result = server
        .engine
        .result(workflow_id, run_id)
        .await?
        .map_err(|error| format!("fixture workflow failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(value, json!(42));
    Ok(())
}

fn wire_error<T: std::fmt::Debug>(result: Result<T, WireError>) -> Result<WireError, TestError> {
    match result {
        Ok(value) => Err(format!("expected a wire error, got {value:?}").into()),
        Err(error) => Ok(error),
    }
}

#[tokio::test]
async fn query_happy_path_returns_handler_payload_through_namespace_guard() -> Result<(), TestError>
{
    let server = Server::over("queryable", QUERY_TIMEOUT).await?;
    let alice = caller_for("alice", TENANT_A);
    let (workflow_id, run_id) = start_parked(&server, &alice, TENANT_A).await?;

    let outcome = query_when_registered(&server, &alice, TENANT_A, &workflow_id, "state").await?;

    let payload = outcome.map_err(|error| format!("expected a result outcome, got {error}"))?;
    let (answer, query_id) = state_reply(&payload)?;
    assert_eq!(answer, 1);
    assert!(!query_id.is_empty(), "handler must observe a query id");

    release_and_complete(&server, &alice, TENANT_A, &workflow_id, &run_id).await?;
    server.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn unknown_query_rides_the_outcome_error_oneof() -> Result<(), TestError> {
    let server = Server::over("queryable", QUERY_TIMEOUT).await?;
    let alice = caller_for("alice", TENANT_A);
    let (workflow_id, run_id) = start_parked(&server, &alice, TENANT_A).await?;
    // Wait for registration first so the unknown-name outcome below is about
    // the name, not about registration timing.
    let registered =
        query_when_registered(&server, &alice, TENANT_A, &workflow_id, "state").await?;
    assert!(
        registered.is_ok(),
        "state query must answer: {registered:?}"
    );

    let response = handlers::query(
        &server.guard,
        &alice,
        named_query_request(TENANT_A, &workflow_id, "missing"),
    )
    .await?;

    let error = wire_error(decode_outcome(response)?)?;
    assert_eq!(error.code, WireErrorCode::UnknownQuery);

    release_and_complete(&server, &alice, TENANT_A, &workflow_id, &run_id).await?;
    server.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn unserviced_query_times_out_as_an_outcome_error() -> Result<(), TestError> {
    // Short reply deadline: the `unpumped` entry parks in a plain Erlang
    // receive with no pump, so a delivered query is never serviced.
    let server = Server::over("unpumped", Duration::from_millis(200)).await?;
    let alice = caller_for("alice", TENANT_A);
    let (workflow_id, run_id) = start_parked(&server, &alice, TENANT_A).await?;

    let outcome = query_when_registered(&server, &alice, TENANT_A, &workflow_id, "state").await?;

    let error = wire_error(outcome)?;
    assert_eq!(error.code, WireErrorCode::QueryTimeout);

    // The workflow still completes cleanly despite the dropped reply: wake
    // the raw receive (it matches the signal wake marker), then release the
    // pumped "finish" await.
    for signal_name in ["wake", "finish"] {
        handlers::signal(
            &server.guard,
            &alice,
            ProtoSignalRequest {
                namespace: TENANT_A.to_owned(),
                workflow_id: Some(workflow_id.clone().into()),
                run_id: Some(run_id.clone().into()),
                signal_name: signal_name.to_owned(),
                payload: Some(Payload::from_json(&json!({ "label": signal_name }))?.into()),
            },
        )
        .await?;
    }
    let result = server
        .engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("workflow failed after query timeout: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(value, json!(42));

    server.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn terminal_workflow_query_is_a_not_running_outcome_error() -> Result<(), TestError> {
    let server = Server::over("queryable", QUERY_TIMEOUT).await?;
    let alice = caller_for("alice", TENANT_A);
    let (workflow_id, run_id) = start_parked(&server, &alice, TENANT_A).await?;
    let registered =
        query_when_registered(&server, &alice, TENANT_A, &workflow_id, "state").await?;
    assert!(
        registered.is_ok(),
        "state query must answer: {registered:?}"
    );
    release_and_complete(&server, &alice, TENANT_A, &workflow_id, &run_id).await?;
    let described = handlers::describe(
        &server.guard,
        &alice,
        aion_proto::ProtoDescribeWorkflowRequest {
            namespace: TENANT_A.to_owned(),
            workflow_id: Some(workflow_id.clone().into()),
            run_id: None,
            include_history: false,
        },
    )
    .await?;
    let summary = described.summary.ok_or("describe summary missing")?;
    let summary = aion_proto::convert::decode_workflow_summary(&summary)?;
    assert_eq!(summary.status, WorkflowStatus::Completed);

    let response =
        handlers::query(&server.guard, &alice, query_request(TENANT_A, &workflow_id)).await?;

    let error = wire_error(decode_outcome(response)?)?;
    assert_eq!(error.code, WireErrorCode::NotRunning);
    assert_eq!(error.error_type.as_deref(), Some("QueryNotRunning"));

    server.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn raising_handler_is_a_query_failed_outcome_error() -> Result<(), TestError> {
    let server = Server::over("queryable", QUERY_TIMEOUT).await?;
    let alice = caller_for("alice", TENANT_A);
    let (workflow_id, run_id) = start_parked(&server, &alice, TENANT_A).await?;
    let registered =
        query_when_registered(&server, &alice, TENANT_A, &workflow_id, "state").await?;
    assert!(
        registered.is_ok(),
        "state query must answer: {registered:?}"
    );

    let response = handlers::query(
        &server.guard,
        &alice,
        named_query_request(TENANT_A, &workflow_id, "boom"),
    )
    .await?;

    let error = wire_error(decode_outcome(response)?)?;
    assert_eq!(error.code, WireErrorCode::QueryFailed);
    assert_eq!(error.error_type.as_deref(), Some("QueryFailed"));
    assert!(
        error.message.contains("fixture boom"),
        "outcome error must carry the handler's raise reason: {}",
        error.message
    );

    // The workflow survived the raise: it still answers and completes.
    let followup = query_when_registered(&server, &alice, TENANT_A, &workflow_id, "state").await?;
    assert!(
        followup.is_ok(),
        "follow-up query must answer: {followup:?}"
    );
    release_and_complete(&server, &alice, TENANT_A, &workflow_id, &run_id).await?;
    server.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn namespace_denials_stay_transport_level_and_anti_leak_is_byte_identical()
-> Result<(), TestError> {
    let server = Server::over("queryable", QUERY_TIMEOUT).await?;
    let alice = caller_for("alice", TENANT_A);
    let bob = caller_for("bob", TENANT_B);
    let (workflow_id, run_id) = start_parked(&server, &alice, TENANT_A).await?;
    let registered =
        query_when_registered(&server, &alice, TENANT_A, &workflow_id, "state").await?;
    assert!(
        registered.is_ok(),
        "state query must answer: {registered:?}"
    );

    // (a) A caller granted nowhere is denied at the transport level before
    // any query outcome exists, even though the target workflow is live.
    let nowhere = handlers::query(
        &server.guard,
        &ungranted_caller(),
        query_request(TENANT_A, &workflow_id),
    )
    .await;
    let nowhere = nowhere.err().ok_or("expected a namespace denial")?;
    assert_eq!(nowhere.code, WireErrorCode::NamespaceDenied);

    // (b) A caller granted elsewhere requesting the foreign namespace is
    // denied identically.
    let cross = handlers::query(&server.guard, &bob, query_request(TENANT_A, &workflow_id)).await;
    let cross = cross.err().ok_or("expected a namespace denial")?;
    assert_eq!(cross.code, WireErrorCode::NamespaceDenied);

    // (c) Anti-leak: a granted caller probing the other tenant's live
    // workflow inside its own namespace gets a transport-level NotFound
    // byte-identical to probing a workflow that never existed.
    let foreign = handlers::query(&server.guard, &bob, query_request(TENANT_B, &workflow_id)).await;
    let foreign = foreign.err().ok_or("expected the anti-leak NotFound")?;
    let absent = handlers::query(
        &server.guard,
        &bob,
        query_request(TENANT_B, &WorkflowId::new(uuid::Uuid::new_v4())),
    )
    .await;
    let absent = absent.err().ok_or("expected NotFound for an absent id")?;
    assert_eq!(foreign.code, WireErrorCode::NotFound);
    assert_eq!(
        foreign.message,
        format!("workflow not found in namespace {TENANT_B}")
    );
    assert_eq!(
        foreign, absent,
        "anti-leak responses must be byte-identical"
    );

    release_and_complete(&server, &alice, TENANT_A, &workflow_id, &run_id).await?;
    server.shutdown()?;
    Ok(())
}
