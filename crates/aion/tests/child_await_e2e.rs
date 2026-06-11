//! Two-phase `await_child` end-to-end tests.
//!
//! These tests drive the converted suspending `await_child` native — the
//! child-terminal watcher, the `aion_child_terminal` wake marker, the
//! record-then-spawn initiation order (#56), the startup recovery sweep,
//! and continue-as-new run-chain transparency — against real BEAM workflow
//! fixtures over a shared `InMemoryStore`. Determinism proofs compare
//! queried/crashed runs against untouched control runs by history shape:
//! envelope timestamps and workflow/child identifiers necessarily differ
//! between runs, while everything deterministic re-execution depends on
//! must not.

use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, EngineError, QueryError, RuntimeHandle, SignalRouter};
use aion_core::{ContentType, Event, EventEnvelope, Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore, WriteToken};
use serde_json::json;

const PARENT_MODULE: &str = "aion_parent_query_fixture";
const PLAIN_PARENT_MODULE: &str = "aion_parent_fixture";
const CHILD_MODULE: &str = "aion_child_fixture";
const PARENT_BEAM: &[u8] = include_bytes!("fixtures/aion_parent_query_fixture.beam");
const PARENT_SOURCE: &[u8] = include_bytes!("fixtures/aion_parent_query_fixture.erl");
const PLAIN_PARENT_BEAM: &[u8] = include_bytes!("fixtures/aion_parent_fixture.beam");
const PLAIN_PARENT_SOURCE: &[u8] = include_bytes!("fixtures/aion_parent_fixture.erl");
const CHILD_BEAM: &[u8] = include_bytes!("fixtures/aion_child_fixture.beam");
const CHILD_SOURCE: &[u8] = include_bytes!("fixtures/aion_child_fixture.erl");

/// Generous engine reply deadline for tests where queries must succeed.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for fixture handler registration (workflow code races the caller).
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(20);

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture_package(
    module: &str,
    beam: &[u8],
    source: &[u8],
    entry_function: &str,
) -> Result<Package, Box<dyn std::error::Error>> {
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
    Ok(Package::load_from_bytes(archive)?)
}

/// Engine over `store` with the pumped parent (`entry`), the plain parent
/// (`child_then_signal`), and the child fixture (`child_entry`) loaded.
async fn engine_over(
    store: &Arc<dyn EventStore>,
    parent_entry: &str,
    child_entry: &str,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .query_timeout(QUERY_TIMEOUT)
        .load_workflows(fixture_package(
            PARENT_MODULE,
            PARENT_BEAM,
            PARENT_SOURCE,
            parent_entry,
        )?)
        .load_workflows(fixture_package(
            PLAIN_PARENT_MODULE,
            PLAIN_PARENT_BEAM,
            PLAIN_PARENT_SOURCE,
            "child_then_signal",
        )?)
        .load_workflows(fixture_package(
            CHILD_MODULE,
            CHILD_BEAM,
            CHILD_SOURCE,
            child_entry,
        )?)
        .build()
        .await?)
}

fn parent_input() -> Result<Payload, Box<dyn std::error::Error>> {
    Ok(Payload::from_json(&json!({ "fixture": "input" }))?)
}

fn signal_payload(label: &str) -> Result<Payload, Box<dyn std::error::Error>> {
    Ok(Payload::from_json(&json!({ "label": label }))?)
}

async fn start_parent(
    engine: &Engine,
    module: &str,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(module, parent_input()?, std::collections::HashMap::new())
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

async fn wait_for_history<F>(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    description: &str,
    predicate: F,
) -> Result<Vec<Event>, Box<dyn std::error::Error>>
where
    F: Fn(&[Event]) -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let history = store.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if std::time::Instant::now() > deadline {
            return Err(format!("timed out waiting for {description}: {history:#?}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn child_started_ids(history: &[Event]) -> Vec<WorkflowId> {
    history
        .iter()
        .filter_map(|event| match event {
            Event::ChildWorkflowStarted {
                child_workflow_id, ..
            } => Some(child_workflow_id.clone()),
            _ => None,
        })
        .collect()
}

fn count_child_completed(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ChildWorkflowCompleted { .. }))
        .count()
}

fn count_workflow_started(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::WorkflowStarted { .. }))
        .count()
}

fn result_json(payload: &Payload) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(payload.bytes())?)
}

/// Project a run history onto its deterministic shape — seq, kind, names,
/// and recorded payload bytes — with child workflow identifiers masked (they
/// are recorded nondeterminism and necessarily differ between runs).
fn run_shape(history: &[Event]) -> Vec<String> {
    history
        .iter()
        .map(|event| match event {
            Event::WorkflowStarted {
                envelope,
                workflow_type,
                input,
                ..
            } => format!(
                "{}|started|{workflow_type}|{}",
                envelope.seq,
                String::from_utf8_lossy(input.bytes())
            ),
            Event::ChildWorkflowStarted {
                envelope,
                workflow_type,
                input,
                ..
            } => format!(
                "{}|child_started|<child>|{workflow_type}|{}",
                envelope.seq,
                String::from_utf8_lossy(input.bytes())
            ),
            Event::ChildWorkflowCompleted {
                envelope, result, ..
            } => format!(
                "{}|child_completed|<child>|{}",
                envelope.seq,
                String::from_utf8_lossy(result.bytes())
            ),
            Event::ChildWorkflowFailed {
                envelope, error, ..
            } => format!("{}|child_failed|<child>|{}", envelope.seq, error.message),
            Event::SignalReceived {
                envelope,
                name,
                payload,
            } => format!(
                "{}|signal|{name}|{}",
                envelope.seq,
                String::from_utf8_lossy(payload.bytes())
            ),
            Event::WorkflowCompleted { envelope, result } => format!(
                "{}|completed|{}",
                envelope.seq,
                String::from_utf8_lossy(result.bytes())
            ),
            other => format!("{}|unexpected|{other:?}", other.seq()),
        })
        .collect()
}

/// Run id recorded by the latest `WorkflowStarted`, for signalling children.
fn latest_run_id(history: &[Event]) -> Result<RunId, Box<dyn std::error::Error>> {
    history
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::WorkflowStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .ok_or_else(|| "history has no WorkflowStarted".into())
}

/// Release the gated child fixture: signal `child_go` to the child run.
async fn release_child(
    engine: &Engine,
    store: &Arc<dyn EventStore>,
    child_id: &WorkflowId,
) -> TestResult {
    let child_history = wait_for_history(store, child_id, "child WorkflowStarted", |events| {
        !events.is_empty()
    })
    .await?;
    let child_run = latest_run_id(&child_history)?;
    engine
        .signal(child_id, &child_run, "child_go", signal_payload("go")?)
        .await?;
    Ok(())
}

/// Query `name`, retrying while the fixture has not yet executed its
/// `register_query` call (registration is workflow code, racing the caller).
async fn query_when_registered(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    name: &str,
) -> Result<Payload, EngineError> {
    let deadline = std::time::Instant::now() + REGISTRATION_DEADLINE;
    loop {
        let outcome = engine.query(workflow_id, run_id, name).await;
        match outcome {
            Err(EngineError::Query(QueryError::UnknownQuery(_)))
                if std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            other => return other,
        }
    }
}

/// Decode the `state` handler's reply payload into `(answer, query_id)`.
fn state_reply(payload: &Payload) -> Result<(i64, String), Box<dyn std::error::Error>> {
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

/// Drive one pumped-parent run to completion: release the child once its
/// terminal is recorded into the parent, then release the parent gate.
async fn complete_parent_run(
    engine: &Engine,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let with_spawn = wait_for_history(store, workflow_id, "child spawn recorded", |events| {
        child_started_ids(events).len() == 1
    })
    .await?;
    let child_id = child_started_ids(&with_spawn)
        .pop()
        .ok_or("missing child id")?;
    release_child(engine, store, &child_id).await?;
    wait_for_history(
        store,
        workflow_id,
        "child terminal recorded into parent history",
        |events| count_child_completed(events) == 1,
    )
    .await?;
    engine
        .signal(workflow_id, run_id, "release", signal_payload("release")?)
        .await?;
    let result = engine
        .result(workflow_id, run_id)
        .await?
        .map_err(|error| format!("parent workflow failed: {error:?}"))?;
    assert_eq!(
        result_json(&result)?,
        json!(42),
        "parent must return the child's terminal value"
    );
    Ok(store.read_history(workflow_id).await?)
}

// --- brief §4 item 4: query a parent parked in await_child (commissioning) --

#[tokio::test]
async fn query_answers_while_parent_is_parked_in_await_child() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "queryable_await", "gated").await?;
    let (workflow_id, run_id) = start_parent(&engine, PARENT_MODULE).await?;

    // The parent is parked inside await_child: its child is spawned and
    // signal-gated, and no terminal exists anywhere.
    let before = wait_for_history(&store, &workflow_id, "child spawn recorded", |events| {
        child_started_ids(events).len() == 1
    })
    .await?;
    assert_eq!(count_child_completed(&before), 0);

    // The engine answers the query through the pump at the await_child
    // yield point.
    let reply = query_when_registered(&engine, &workflow_id, &run_id, "state").await?;
    let (answer, query_id) = state_reply(&reply)?;
    assert_eq!(answer, 1);
    assert!(!query_id.is_empty(), "handler must observe a query id");
    // Byte-identical history before and after the query: count and content.
    assert_eq!(
        store.read_history(&workflow_id).await?,
        before,
        "the query path must never append events"
    );

    // Release the child, then the parent; the run completes normally.
    let queried_history = complete_parent_run(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;

    // Control: identical inputs and signals, never queried.
    let control_store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let control = engine_over(&control_store, "queryable_await", "gated").await?;
    let (control_id, control_run) = start_parent(&control, PARENT_MODULE).await?;
    let control_history =
        complete_parent_run(&control, &control_store, &control_id, &control_run).await?;
    control.shutdown()?;

    assert_eq!(
        run_shape(&queried_history),
        run_shape(&control_history),
        "a queried run's history must be shape-identical to the never-queried control"
    );
    Ok(())
}

// --- brief §4 item 6a: crash mid-await, child still running ------------------

#[tokio::test]
async fn crash_mid_await_child_recovers_and_matches_uncrashed_control() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let first = engine_over(&store, "await_gated", "gated").await?;
    let (workflow_id, run_id) = start_parent(&first, PARENT_MODULE).await?;

    // Crash with the parent parked in await_child and the child still
    // running: the spawn is durable, no terminal exists anywhere.
    let pre_crash = wait_for_history(&store, &workflow_id, "child spawn recorded", |events| {
        child_started_ids(events).len() == 1
    })
    .await?;
    let child_id = child_started_ids(&pre_crash).pop().ok_or("missing child")?;
    wait_for_history(&store, &child_id, "child WorkflowStarted", |events| {
        !events.is_empty()
    })
    .await?;
    assert_eq!(count_child_completed(&pre_crash), 0);
    first.shutdown()?;

    // Recovery re-spawns parent and child; the parent's replay re-arms the
    // watcher; releasing the child completes the chain.
    let recovered = engine_over(&store, "await_gated", "gated").await?;
    release_child(&recovered, &store, &child_id).await?;
    wait_for_history(
        &store,
        &workflow_id,
        "child terminal recorded into recovered parent history",
        |events| count_child_completed(events) == 1,
    )
    .await?;
    recovered
        .signal(&workflow_id, &run_id, "release", signal_payload("release")?)
        .await?;
    let result = recovered
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("recovered parent failed: {error:?}"))?;
    assert_eq!(result_json(&result)?, json!(42));
    let crashed_history = store.read_history(&workflow_id).await?;
    assert_eq!(
        child_started_ids(&crashed_history),
        vec![child_id],
        "recovery must not respawn or duplicate the child: {crashed_history:#?}"
    );
    recovered.shutdown()?;

    // Uncrashed control run with identical inputs and signal order.
    let control_store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let control = engine_over(&control_store, "await_gated", "gated").await?;
    let (control_id, control_run) = start_parent(&control, PARENT_MODULE).await?;
    let control_history =
        complete_parent_run(&control, &control_store, &control_id, &control_run).await?;
    control.shutdown()?;

    assert_eq!(
        run_shape(&crashed_history),
        run_shape(&control_history),
        "the crashed-and-recovered run must be shape-identical to the uncrashed control"
    );
    Ok(())
}

// --- brief §4 item 6b: same, queried before the crash and after recovery -----

#[tokio::test]
async fn queried_crash_recovery_matches_unqueried_uncrashed_control() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let first = engine_over(&store, "queryable_await", "gated").await?;
    let (workflow_id, run_id) = start_parent(&first, PARENT_MODULE).await?;
    let pre_crash = wait_for_history(&store, &workflow_id, "child spawn recorded", |events| {
        child_started_ids(events).len() == 1
    })
    .await?;
    let child_id = child_started_ids(&pre_crash).pop().ok_or("missing child")?;
    wait_for_history(&store, &child_id, "child WorkflowStarted", |events| {
        !events.is_empty()
    })
    .await?;

    // Query the parent while it is parked in await_child, then crash.
    let (answer, _) =
        state_reply(&query_when_registered(&first, &workflow_id, &run_id, "state").await?)?;
    assert_eq!(answer, 1);
    assert_eq!(
        store.read_history(&workflow_id).await?,
        pre_crash,
        "pre-crash queries must append nothing"
    );
    first.shutdown()?;

    // Query again after recovery (replay re-registered the handler), then
    // complete the run.
    let recovered = engine_over(&store, "queryable_await", "gated").await?;
    let (answer, _) =
        state_reply(&query_when_registered(&recovered, &workflow_id, &run_id, "state").await?)?;
    assert_eq!(answer, 1);
    assert_eq!(
        store.read_history(&workflow_id).await?,
        pre_crash,
        "neither recovery replay nor queries may append or rewrite events"
    );
    release_child(&recovered, &store, &child_id).await?;
    wait_for_history(
        &store,
        &workflow_id,
        "child terminal recorded into recovered parent history",
        |events| count_child_completed(events) == 1,
    )
    .await?;
    recovered
        .signal(&workflow_id, &run_id, "release", signal_payload("release")?)
        .await?;
    let result = recovered
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("recovered parent failed: {error:?}"))?;
    assert_eq!(result_json(&result)?, json!(42));
    let queried_crashed = store.read_history(&workflow_id).await?;
    recovered.shutdown()?;

    // Control: same workflow code, never queried, never crashed.
    let control_store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let control = engine_over(&control_store, "queryable_await", "gated").await?;
    let (control_id, control_run) = start_parent(&control, PARENT_MODULE).await?;
    let control_history =
        complete_parent_run(&control, &control_store, &control_id, &control_run).await?;
    control.shutdown()?;

    assert_eq!(
        run_shape(&queried_crashed),
        run_shape(&control_history),
        "queried/crashed and unqueried/uncrashed histories must agree in shape"
    );
    Ok(())
}

/// Synthesize the crash-window parent history: `WorkflowStarted` plus one
/// recorded `ChildWorkflowStarted` for `child_workflow_id`, nothing else.
async fn synthesize_parent_with_recorded_spawn(
    store: &Arc<dyn EventStore>,
    parent_workflow_id: &WorkflowId,
    parent_run_id: &RunId,
    child_workflow_id: &WorkflowId,
) -> TestResult {
    let recorded_at = chrono::Utc::now();
    store
        .append(
            WriteToken::recorder(),
            parent_workflow_id,
            &[
                Event::WorkflowStarted {
                    envelope: EventEnvelope {
                        seq: 1,
                        recorded_at,
                        workflow_id: parent_workflow_id.clone(),
                    },
                    workflow_type: PLAIN_PARENT_MODULE.to_owned(),
                    input: parent_input()?,
                    run_id: parent_run_id.clone(),
                    parent_run_id: None,
                },
                Event::ChildWorkflowStarted {
                    envelope: EventEnvelope {
                        seq: 2,
                        recorded_at,
                        workflow_id: parent_workflow_id.clone(),
                    },
                    child_workflow_id: child_workflow_id.clone(),
                    workflow_type: CHILD_MODULE.to_owned(),
                    input: Payload::new(ContentType::Json, br#""child-input""#.to_vec()),
                },
            ],
            0,
        )
        .await?;
    Ok(())
}

/// Synthesize an already-terminal child history: started, then completed
/// with the fixture result `42`.
async fn synthesize_completed_child(
    store: &Arc<dyn EventStore>,
    child_workflow_id: &WorkflowId,
) -> TestResult {
    let recorded_at = chrono::Utc::now();
    store
        .append(
            WriteToken::recorder(),
            child_workflow_id,
            &[
                Event::WorkflowStarted {
                    envelope: EventEnvelope {
                        seq: 1,
                        recorded_at,
                        workflow_id: child_workflow_id.clone(),
                    },
                    workflow_type: CHILD_MODULE.to_owned(),
                    input: Payload::new(ContentType::Json, br#""child-input""#.to_vec()),
                    run_id: RunId::new_v4(),
                    parent_run_id: None,
                },
                Event::WorkflowCompleted {
                    envelope: EventEnvelope {
                        seq: 2,
                        recorded_at,
                        workflow_id: child_workflow_id.clone(),
                    },
                    result: Payload::from_json(&json!(42))?,
                },
            ],
            0,
        )
        .await?;
    Ok(())
}

// --- brief §4 item 6c: child terminal durable, parent-side record missing ----

#[tokio::test]
async fn watcher_resolves_terminal_child_with_no_handle_from_the_store() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    // Synthesize the crash point: the parent recorded the spawn and was
    // parked awaiting; the child completed durably in its own history; the
    // parent-side ChildWorkflowCompleted was never recorded. After restart
    // the terminal child gets no registry handle, so only the watcher's
    // store-truth read can resolve the await.
    let parent_workflow_id = WorkflowId::new_v4();
    let parent_run_id = RunId::new_v4();
    let child_workflow_id = WorkflowId::new_v4();
    synthesize_parent_with_recorded_spawn(
        &store,
        &parent_workflow_id,
        &parent_run_id,
        &child_workflow_id,
    )
    .await?;
    synthesize_completed_child(&store, &child_workflow_id).await?;
    let child_history_len = store.read_history(&child_workflow_id).await?.len();

    let engine = engine_over(&store, "await_gated", "complete").await?;
    // The recovered parent replays to await_child; its watcher reads the
    // child terminal from the store (the child has no handle) and records
    // the parent-side terminal.
    wait_for_history(
        &store,
        &parent_workflow_id,
        "watcher-recorded child terminal",
        |events| count_child_completed(events) == 1,
    )
    .await?;
    engine
        .signal(
            &parent_workflow_id,
            &parent_run_id,
            "release",
            signal_payload("release")?,
        )
        .await?;
    let result = engine
        .result(&parent_workflow_id, &parent_run_id)
        .await?
        .map_err(|error| format!("recovered parent failed: {error:?}"))?;

    assert_eq!(
        result_json(&result)?,
        json!([child_workflow_id.to_string(), 42]),
        "the await must resolve the recorded child id's stored terminal"
    );
    let final_history = store.read_history(&parent_workflow_id).await?;
    assert_eq!(
        child_started_ids(&final_history),
        vec![child_workflow_id.clone()]
    );
    assert_eq!(count_child_completed(&final_history), 1);
    assert_eq!(
        store.read_history(&child_workflow_id).await?.len(),
        child_history_len,
        "the terminal child's own history must be untouched"
    );
    engine.shutdown()?;
    Ok(())
}

// --- brief §4 item 7: #56 recorded-but-never-spawned window -------------------

#[tokio::test]
async fn recovery_sweep_starts_recorded_but_never_spawned_child_exactly_once() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    // Synthesize the record-then-spawn crash window: the parent durably
    // recorded ChildWorkflowStarted but the engine died before the child
    // process (or its history) ever existed.
    let parent_workflow_id = WorkflowId::new_v4();
    let parent_run_id = RunId::new_v4();
    let child_workflow_id = WorkflowId::new_v4();
    synthesize_parent_with_recorded_spawn(
        &store,
        &parent_workflow_id,
        &parent_run_id,
        &child_workflow_id,
    )
    .await?;
    assert!(store.read_history(&child_workflow_id).await?.is_empty());

    // Startup recovery sweeps the recovered parent's run segment and starts
    // the child under the recorded id; the parent's await then completes.
    let engine = engine_over(&store, "await_gated", "complete").await?;
    wait_for_history(
        &store,
        &parent_workflow_id,
        "swept child terminal recorded into parent history",
        |events| count_child_completed(events) == 1,
    )
    .await?;
    engine
        .signal(
            &parent_workflow_id,
            &parent_run_id,
            "release",
            signal_payload("release")?,
        )
        .await?;
    let result = engine
        .result(&parent_workflow_id, &parent_run_id)
        .await?
        .map_err(|error| format!("recovered parent failed: {error:?}"))?;
    assert_eq!(
        result_json(&result)?,
        json!([child_workflow_id.to_string(), 42]),
        "the sweep-started child must complete under the recorded identity"
    );

    // Exactly one child execution exists, under exactly the recorded id.
    let child_history = store.read_history(&child_workflow_id).await?;
    assert_eq!(
        count_workflow_started(&child_history),
        1,
        "the sweep must start exactly one child: {child_history:#?}"
    );
    let parent_history = store.read_history(&parent_workflow_id).await?;
    assert_eq!(
        child_started_ids(&parent_history),
        vec![child_workflow_id.clone()],
        "no duplicate ChildWorkflowStarted may exist: {parent_history:#?}"
    );
    engine.shutdown()?;

    // A second restart changes nothing: the parent is terminal and the
    // child's history is non-empty, so the sweep is a no-op.
    let again = engine_over(&store, "await_gated", "complete").await?;
    assert_eq!(
        count_workflow_started(&store.read_history(&child_workflow_id).await?),
        1,
        "an idempotent sweep must not start a second child"
    );
    assert_eq!(
        child_started_ids(&store.read_history(&parent_workflow_id).await?),
        vec![child_workflow_id]
    );
    again.shutdown()?;
    Ok(())
}

// --- brief §4 item 8: continue-as-new child transparency ----------------------

#[tokio::test]
async fn await_child_follows_continue_as_new_chain_and_survives_restart() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "await_gated", "can_once").await?;
    let handle = engine
        .start_workflow(
            PLAIN_PARENT_MODULE,
            parent_input()?,
            std::collections::HashMap::new(),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();

    // The child rotates once via continue-as-new; the await must resolve
    // with the final run's result against the stable child id.
    let with_terminal = wait_for_history(
        &store,
        &workflow_id,
        "final-run child terminal recorded into parent history",
        |events| count_child_completed(events) == 1,
    )
    .await?;
    let recorded_children = child_started_ids(&with_terminal);
    assert_eq!(recorded_children.len(), 1, "history: {with_terminal:#?}");
    let child_id = recorded_children[0].clone();
    let terminal_child_id = with_terminal
        .iter()
        .find_map(|event| match event {
            Event::ChildWorkflowCompleted {
                child_workflow_id, ..
            } => Some(child_workflow_id.clone()),
            _ => None,
        })
        .ok_or("missing parent-side child terminal")?;
    assert_eq!(
        terminal_child_id, child_id,
        "the recorded terminal must carry the stable child workflow id"
    );
    let child_history = store.read_history(&child_id).await?;
    assert_eq!(
        count_workflow_started(&child_history),
        2,
        "one rotation: original run plus replacement: {child_history:#?}"
    );
    engine.shutdown()?;

    // Crash/restart with the parent still gated: replay must return the
    // same result with zero new spawns and zero new child runs.
    let recovered = engine_over(&store, "await_gated", "can_once").await?;
    recovered
        .signal(&workflow_id, &run_id, "release", signal_payload("release")?)
        .await?;
    let result = recovered
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("recovered parent failed: {error:?}"))?;
    assert_eq!(
        result_json(&result)?,
        json!([child_id.to_string(), 42]),
        "the await must resolve with the final run's result"
    );
    assert_eq!(
        child_started_ids(&store.read_history(&workflow_id).await?),
        vec![child_id.clone()],
        "restart replay must not spawn again"
    );
    assert_eq!(
        count_workflow_started(&store.read_history(&child_id).await?),
        2,
        "restart replay must not start any new child run"
    );
    recovered.shutdown()?;
    Ok(())
}
