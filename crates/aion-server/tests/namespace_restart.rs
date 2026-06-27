//! Restart-durability coverage for the namespace access-control model.
//!
//! Workflow and schedule ownership are projections of durable history, so a
//! server rebuilt over the same database must enforce exactly the same
//! authorization matrix as the instance that recorded the resources:
//!
//! - the owning namespace's caller can still describe its workflow/schedule;
//! - a granted caller probing a foreign-owned id receives the anti-leak
//!   `NotFound`, byte-identical to probing an id that never existed;
//! - a caller without a grant for the requested namespace receives
//!   `NamespaceDenied`.
//!
//! "Restart" rebuilds the store handle, engine, resolver, and guard from
//! scratch over the same libSQL file, so the ownership projection is genuinely
//! re-derived from persisted events — no in-memory state crosses the boundary.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aion::{Engine, EngineBuilder};
use aion_core::{
    CatchUpPolicy, Event, OverlapPolicy, Payload, RunId, ScheduleConfig, ScheduleId,
    SearchAttributeSchema, SearchAttributeType, SearchAttributeValue, TriggerSpec, WorkflowId,
    WorkflowStatus, search_attributes_from_events,
};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_proto::convert::{decode_schedule_state, decode_workflow_summary, encode_schedule_config};
use aion_proto::{
    ProtoCreateScheduleRequest, ProtoDescribeScheduleResponse, ProtoDescribeWorkflowRequest,
    ProtoDescribeWorkflowResponse, ProtoScheduleIdRequest, ProtoStartWorkflowRequest, WireError,
    WireErrorCode,
};
use aion_server::api::{handlers, schedule_handlers};
use aion_server::config::{NamespaceConfig, NamespaceMode};
use aion_server::{CallerIdentity, NAMESPACE_ATTRIBUTE, NamespaceGuard, NamespaceResolver};
use aion_store::EventStore;
use aion_store_libsql::LibSqlStore;
use serde_json::json;

type TestError = Box<dyn std::error::Error>;

const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";

const FIXTURE_MODULE: &str = "aion_fixture_workflow";
const FIXTURE_BEAM: &[u8] = include_bytes!("../../aion/tests/fixtures/aion_fixture_workflow.beam");
const FIXTURE_SOURCE: &[u8] = include_bytes!("../../aion/tests/fixtures/aion_fixture_workflow.erl");

/// Parent fixture that spawns one `aion_fixture_workflow` child through the
/// engine's real child NIF path and awaits it (see `fixtures/README.md`).
const PARENT_MODULE: &str = "aion_fixture_parent";
const PARENT_BEAM: &[u8] = include_bytes!("fixtures/aion_fixture_parent.beam");
const PARENT_SOURCE: &[u8] = include_bytes!("fixtures/aion_fixture_parent.erl");

/// One in-process "server": a real engine over a persistent store plus the
/// production resolver/guard wiring (`NamespaceResolver::from_config` installs
/// the durable history ownership sources, exactly as `ServerState` does).
struct Server {
    engine: Arc<Engine>,
    guard: NamespaceGuard,
}

impl Server {
    async fn over(db_path: &Path, packages: Vec<Package>) -> Result<Self, TestError> {
        let store: Arc<dyn EventStore> = Arc::new(LibSqlStore::open(db_path.to_path_buf()).await?);
        let mut schema = SearchAttributeSchema::new();
        schema.register(NAMESPACE_ATTRIBUTE, SearchAttributeType::String)?;
        let mut builder = EngineBuilder::new()
            .store_arc(store)
            .in_memory_visibility()
            .search_attribute_schema(schema)
            .scheduler_threads(1);
        for package in packages {
            builder = builder.load_workflows(package);
        }
        let engine = Arc::new(builder.build().await?);
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

fn package_from(
    module: &str,
    beam: &[u8],
    source: &[u8],
    entry_function: &str,
) -> Result<Package, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(module, beam)])?;
    let manifest = Manifest {
        entry_module: module.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let archive = PackageBuilder::with_source(manifest, beams, [(module, source.to_vec())])
        .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

fn fixture_package(entry_function: &str) -> Result<Package, TestError> {
    package_from(FIXTURE_MODULE, FIXTURE_BEAM, FIXTURE_SOURCE, entry_function)
}

/// The parent fixture plus the child type it spawns; the child package's
/// `complete` entry makes spawned children finish immediately.
fn parent_and_child_packages() -> Result<Vec<Package>, TestError> {
    Ok(vec![
        package_from(PARENT_MODULE, PARENT_BEAM, PARENT_SOURCE, "orchestrate")?,
        fixture_package("complete")?,
    ])
}

fn caller_for(subject: &str, namespace: &str) -> CallerIdentity {
    CallerIdentity::new(subject, [namespace.to_owned()])
}

fn ungranted_caller() -> CallerIdentity {
    CallerIdentity::new("mallory", Vec::<String>::new())
}

fn start_request(
    namespace: &str,
    workflow_type: &str,
    input: &serde_json::Value,
) -> Result<ProtoStartWorkflowRequest, TestError> {
    Ok(ProtoStartWorkflowRequest {
        namespace: namespace.to_owned(),
        workflow_type: workflow_type.to_owned(),
        input: Some(Payload::from_json(input)?.into()),
        routing_key: None,
    })
}

fn describe_request(namespace: &str, workflow_id: &WorkflowId) -> ProtoDescribeWorkflowRequest {
    ProtoDescribeWorkflowRequest {
        namespace: namespace.to_owned(),
        workflow_id: Some(workflow_id.clone().into()),
        run_id: None,
        include_history: false,
    }
}

fn create_schedule_request(namespace: &str) -> Result<ProtoCreateScheduleRequest, TestError> {
    let config = ScheduleConfig {
        trigger: TriggerSpec::Interval {
            period: Duration::from_secs(3600),
        },
        overlap_policy: OverlapPolicy::Skip,
        catch_up_policy: CatchUpPolicy::Skip,
        workflow_type: FIXTURE_MODULE.to_owned(),
        input: Payload::from_json(&json!({ "fixture": true }))?,
        search_attributes: HashMap::new(),
    };
    Ok(ProtoCreateScheduleRequest {
        namespace: namespace.to_owned(),
        config: Some(encode_schedule_config(namespace, None, &config)?),
    })
}

fn schedule_id_request(namespace: &str, schedule_id: &ScheduleId) -> ProtoScheduleIdRequest {
    ProtoScheduleIdRequest {
        namespace: namespace.to_owned(),
        schedule_id: Some(schedule_id.clone().into()),
    }
}

/// Starts the fixture workflow through the real start handler (which stamps
/// the authorized namespace durably) and awaits its terminal result so the
/// pre-restart shutdown is clean.
async fn start_and_complete(
    server: &Server,
    caller: &CallerIdentity,
    namespace: &str,
) -> Result<WorkflowId, TestError> {
    let response = handlers::start(
        &server.guard,
        caller,
        start_request(namespace, FIXTURE_MODULE, &json!({ "fixture": "input" }))?,
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
    server
        .engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("fixture workflow failed: {error:?}"))?;
    Ok(workflow_id)
}

async fn create_schedule_in(
    server: &Server,
    caller: &CallerIdentity,
    namespace: &str,
) -> Result<ScheduleId, TestError> {
    let response = schedule_handlers::create_schedule(
        &server.guard,
        caller,
        create_schedule_request(namespace)?,
    )
    .await?;
    Ok(response
        .schedule_id
        .ok_or("create response missing schedule id")?
        .try_into()?)
}

fn wire_error<T: std::fmt::Debug>(result: Result<T, WireError>) -> Result<WireError, TestError> {
    match result {
        Ok(value) => Err(format!("expected a wire error, got {value:?}").into()),
        Err(error) => Ok(error),
    }
}

fn described_status(response: &ProtoDescribeWorkflowResponse) -> Result<WorkflowStatus, TestError> {
    let envelope = response
        .summary
        .as_ref()
        .ok_or("describe response missing summary")?;
    Ok(decode_workflow_summary(envelope)?.status)
}

fn described_schedule_owner(
    response: &ProtoDescribeScheduleResponse,
) -> Result<Option<SearchAttributeValue>, TestError> {
    let envelope = response
        .state
        .as_ref()
        .ok_or("describe response missing schedule state")?;
    let state: aion::schedule::ScheduleState = decode_schedule_state(envelope)?;
    Ok(state
        .config
        .search_attributes
        .get(NAMESPACE_ATTRIBUTE)
        .cloned())
}

#[tokio::test]
async fn workflow_namespace_enforcement_survives_restart() -> Result<(), TestError> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("namespace-restart-workflows.db");
    let alice = caller_for("alice", TENANT_A);
    let bob = caller_for("bob", TENANT_B);

    // First server instance: record one workflow per tenant, then shut down
    // cleanly.
    let first = Server::over(&db_path, vec![fixture_package("complete")?]).await?;
    let workflow_a = start_and_complete(&first, &alice, TENANT_A).await?;
    let workflow_b = start_and_complete(&first, &bob, TENANT_B).await?;
    first.shutdown()?;

    // Fresh store handle, engine, resolver, and guard over the same database:
    // ownership must be re-derived from persisted history alone.
    let restarted = Server::over(&db_path, vec![fixture_package("complete")?]).await?;

    // (a) Each owner still describes its own workflow.
    let described_a = handlers::describe(
        &restarted.guard,
        &alice,
        describe_request(TENANT_A, &workflow_a),
    )
    .await?;
    assert_eq!(described_status(&described_a)?, WorkflowStatus::Completed);
    let described_b = handlers::describe(
        &restarted.guard,
        &bob,
        describe_request(TENANT_B, &workflow_b),
    )
    .await?;
    assert_eq!(described_status(&described_b)?, WorkflowStatus::Completed);

    // (b) A granted caller probing the other tenant's id gets the anti-leak
    // NotFound, byte-identical to probing an id that never existed.
    let foreign = wire_error(
        handlers::describe(
            &restarted.guard,
            &alice,
            describe_request(TENANT_A, &workflow_b),
        )
        .await,
    )?;
    let absent = wire_error(
        handlers::describe(
            &restarted.guard,
            &alice,
            describe_request(TENANT_A, &WorkflowId::new(uuid::Uuid::new_v4())),
        )
        .await,
    )?;
    assert_eq!(foreign.code, WireErrorCode::NotFound);
    assert_eq!(
        foreign.message,
        format!("workflow not found in namespace {TENANT_A}")
    );
    assert_eq!(foreign, absent);

    // (c) No grant for the requested namespace is NamespaceDenied — both for
    // a caller granted elsewhere and for a caller granted nowhere — even when
    // the target really exists in that namespace.
    let cross = wire_error(
        handlers::describe(
            &restarted.guard,
            &alice,
            describe_request(TENANT_B, &workflow_b),
        )
        .await,
    )?;
    assert_eq!(cross.code, WireErrorCode::NamespaceDenied);
    let nowhere = wire_error(
        handlers::describe(
            &restarted.guard,
            &ungranted_caller(),
            describe_request(TENANT_A, &workflow_a),
        )
        .await,
    )?;
    assert_eq!(nowhere.code, WireErrorCode::NamespaceDenied);

    restarted.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn schedule_namespace_enforcement_survives_restart() -> Result<(), TestError> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("namespace-restart-schedules.db");
    let alice = caller_for("alice", TENANT_A);
    let bob = caller_for("bob", TENANT_B);

    // First server instance: record one schedule per tenant (the create
    // handler force-stamps the authorized namespace into the recorded
    // config), then shut down cleanly.
    let first = Server::over(&db_path, vec![fixture_package("complete")?]).await?;
    let schedule_a = create_schedule_in(&first, &alice, TENANT_A).await?;
    let schedule_b = create_schedule_in(&first, &bob, TENANT_B).await?;
    first.shutdown()?;

    // Fresh instance over the same database: schedule ownership must be
    // re-derived from the coordinator's persisted ScheduleCreated events.
    let restarted = Server::over(&db_path, vec![fixture_package("complete")?]).await?;

    // (a) Each owner still describes its own schedule, and the described
    // state carries the creation-stamped owner namespace.
    let described_a = schedule_handlers::describe_schedule(
        &restarted.guard,
        &alice,
        schedule_id_request(TENANT_A, &schedule_a),
    )
    .await?;
    assert_eq!(
        described_schedule_owner(&described_a)?,
        Some(SearchAttributeValue::String(TENANT_A.to_owned()))
    );
    let described_b = schedule_handlers::describe_schedule(
        &restarted.guard,
        &bob,
        schedule_id_request(TENANT_B, &schedule_b),
    )
    .await?;
    assert_eq!(
        described_schedule_owner(&described_b)?,
        Some(SearchAttributeValue::String(TENANT_B.to_owned()))
    );

    // (b) A granted caller probing the other tenant's schedule id gets the
    // anti-leak NotFound, byte-identical to probing a nonexistent id.
    let foreign = wire_error(
        schedule_handlers::describe_schedule(
            &restarted.guard,
            &alice,
            schedule_id_request(TENANT_A, &schedule_b),
        )
        .await,
    )?;
    let absent = wire_error(
        schedule_handlers::describe_schedule(
            &restarted.guard,
            &alice,
            schedule_id_request(TENANT_A, &ScheduleId::new(uuid::Uuid::new_v4())),
        )
        .await,
    )?;
    assert_eq!(foreign.code, WireErrorCode::NotFound);
    assert_eq!(
        foreign.message,
        format!("schedule not found in namespace {TENANT_A}")
    );
    assert_eq!(foreign, absent);

    // (c) No grant for the requested namespace is NamespaceDenied — both for
    // a caller granted elsewhere and for a caller granted nowhere.
    let cross = wire_error(
        schedule_handlers::describe_schedule(
            &restarted.guard,
            &alice,
            schedule_id_request(TENANT_B, &schedule_b),
        )
        .await,
    )?;
    assert_eq!(cross.code, WireErrorCode::NamespaceDenied);
    let nowhere = wire_error(
        schedule_handlers::describe_schedule(
            &restarted.guard,
            &ungranted_caller(),
            schedule_id_request(TENANT_A, &schedule_a),
        )
        .await,
    )?;
    assert_eq!(nowhere.code, WireErrorCode::NamespaceDenied);

    restarted.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn child_workflow_namespace_enforcement_survives_restart() -> Result<(), TestError> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("namespace-restart-children.db");
    let alice = caller_for("alice", TENANT_A);
    let bob = caller_for("bob", TENANT_B);

    // First server instance: a tenant-a parent spawns a real child workflow
    // through the engine's child NIF path, which inherits the parent's
    // recorded search attributes — including the namespace stamp.
    let first = Server::over(&db_path, parent_and_child_packages()?).await?;
    let response = handlers::start(
        &first.guard,
        &alice,
        start_request(TENANT_A, PARENT_MODULE, &json!({ "fixture": "input" }))?,
    )
    .await?;
    let parent_id: WorkflowId = response
        .workflow_id
        .ok_or("start response missing workflow id")?
        .try_into()?;
    let parent_run: RunId = response
        .run_id
        .ok_or("start response missing run id")?
        .try_into()?;
    first
        .engine
        .result(&parent_id, &parent_run)
        .await?
        .map_err(|error| format!("parent fixture failed: {error:?}"))?;

    let parent_history = first.engine.store().read_history(&parent_id).await?;
    let child_id = parent_history
        .iter()
        .find_map(|event| match event {
            Event::ChildWorkflowStarted {
                child_workflow_id, ..
            } => Some(child_workflow_id.clone()),
            _ => None,
        })
        .ok_or("parent history recorded no ChildWorkflowStarted event")?;

    // The child's own durable history must carry the inherited namespace
    // stamp — that recorded attribute is the only thing ownership can be
    // re-derived from after restart.
    let child_history = first.engine.store().read_history(&child_id).await?;
    assert_eq!(
        search_attributes_from_events(&child_history).get(NAMESPACE_ATTRIBUTE),
        Some(&SearchAttributeValue::String(TENANT_A.to_owned()))
    );
    first.shutdown()?;

    // Fresh instance over the same database.
    let restarted = Server::over(&db_path, parent_and_child_packages()?).await?;

    // (a) The parent's namespace owns the child after restart.
    let described = handlers::describe(
        &restarted.guard,
        &alice,
        describe_request(TENANT_A, &child_id),
    )
    .await?;
    assert_eq!(described_status(&described)?, WorkflowStatus::Completed);

    // (b) A foreign tenant's probe of the child id is byte-identical to a
    // probe of an id that never existed.
    let foreign = wire_error(
        handlers::describe(
            &restarted.guard,
            &bob,
            describe_request(TENANT_B, &child_id),
        )
        .await,
    )?;
    let absent = wire_error(
        handlers::describe(
            &restarted.guard,
            &bob,
            describe_request(TENANT_B, &WorkflowId::new(uuid::Uuid::new_v4())),
        )
        .await,
    )?;
    assert_eq!(foreign.code, WireErrorCode::NotFound);
    assert_eq!(
        foreign.message,
        format!("workflow not found in namespace {TENANT_B}")
    );
    assert_eq!(foreign, absent);

    // (c) No grant for the parent's namespace is NamespaceDenied.
    let denied = wire_error(
        handlers::describe(
            &restarted.guard,
            &bob,
            describe_request(TENANT_A, &child_id),
        )
        .await,
    )?;
    assert_eq!(denied.code, WireErrorCode::NamespaceDenied);

    restarted.shutdown()?;
    Ok(())
}
