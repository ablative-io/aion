//! Engine query end-to-end tests over the yield-point pump protocol.
//!
//! These tests drive the production query path — `Engine::query` through
//! `ConcreteQueryService`, the query mailbox delivery, the `aion_query` wake
//! marker, and the suspending-await sentinel entry checks — against a real
//! BEAM workflow fixture (`fixtures/aion_fixture_query.erl`) that hand-rolls
//! the SDK pump loop, proving the raw sentinel protocol. Every determinism
//! assertion compares full event vectors: queries must never append history.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{
    Engine, EngineBuilder, EngineError, HandleResidency, QueryError, RuntimeHandle, SignalRouter,
};
use aion_core::{Event, Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

const QUERY_MODULE: &str = "aion_fixture_query";
const QUERY_BEAM: &[u8] = include_bytes!("fixtures/aion_fixture_query.beam");
const QUERY_SOURCE: &[u8] = include_bytes!("fixtures/aion_fixture_query.erl");

/// Generous engine reply deadline for tests where queries must succeed.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for the fixture to finish registering its handlers (the
/// registration NIF runs asynchronously after `start_workflow` returns).
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(20);

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn query_package(entry_function: &str) -> Result<Package, Box<dyn std::error::Error>> {
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
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(QUERY_MODULE, QUERY_SOURCE.to_vec())])
            .write_to_bytes()?;
    Ok(Package::load_from_bytes(archive)?)
}

async fn engine_over(
    store: &Arc<dyn EventStore>,
    entry_function: &str,
    query_timeout: Duration,
) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .query_timeout(query_timeout)
        .load_workflows(query_package(entry_function)?)
        .build()
        .await?)
}

fn fixture_input() -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "fixture": "input" }))
}

fn signal_payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "label": label }))
}

async fn start(engine: &Engine) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            QUERY_MODULE,
            fixture_input()?,
            std::collections::HashMap::new(),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

/// Query `name`, retrying while the fixture has not yet executed its
/// `register_query` calls (registration is workflow code, so it races the
/// caller after `start_workflow`/recovery returns). The first non-
/// `UnknownQuery` outcome — success or any other typed error — is returned.
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
            Err(EngineError::Query(QueryError::ReplyDropped)) => {
                // ReplyDropped on a workflow that should be parked usually
                // means its process died under the engine; the recorded
                // terminal (if any) carries the death cause, so surface
                // durable history alongside the error.
                let history = engine.store().read_history(workflow_id).await;
                eprintln!(
                    "query_when_registered({name}) observed ReplyDropped; history: {history:#?}"
                );
                return Err(EngineError::Query(QueryError::ReplyDropped));
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

fn result_json(payload: &Payload) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(payload.bytes())?)
}

fn event_kind(event: &Event) -> String {
    match event {
        Event::WorkflowStarted { .. } => "workflow_started".to_owned(),
        Event::SignalReceived { .. } => "signal_received".to_owned(),
        Event::WorkflowCompleted { .. } => "workflow_completed".to_owned(),
        other => format!("unexpected:{other:?}"),
    }
}

fn event_kinds(history: &[Event]) -> Vec<String> {
    history.iter().map(event_kind).collect()
}

/// Project a run history onto its deterministic shape — seq, kind, and the
/// recorded payload bytes — for cross-run comparison. Envelope timestamps
/// and workflow/run identifiers necessarily differ between runs; everything
/// the workflow's deterministic re-execution depends on must not.
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

async fn release_and_await_42(
    engine: &Engine,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> TestResult {
    if let Err(error) = engine
        .signal(workflow_id, run_id, "release", signal_payload("release")?)
        .await
    {
        // A delivery failure here means the workflow process died under the
        // engine; the recorded terminal carries the death cause, so surface
        // the durable history alongside the delivery error.
        let history = store.read_history(workflow_id).await?;
        return Err(format!("release signal failed: {error:?}; history: {history:#?}").into());
    }
    let result = engine
        .result(workflow_id, run_id)
        .await?
        .map_err(|error| format!("workflow failed: {error:?}"))?;
    assert_eq!(result_json(&result)?, json!(42));
    Ok(())
}

// --- test plan item 5: happy path + determinism ---------------------------

#[tokio::test]
async fn query_answers_at_yield_point_without_touching_history() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "queryable", QUERY_TIMEOUT).await?;
    let (workflow_id, run_id) = start(&engine).await?;
    let before = store.read_history(&workflow_id).await?;
    assert_eq!(
        event_kinds(&before),
        vec!["workflow_started"],
        "handler registration must not record events"
    );

    let reply = query_when_registered(&engine, &workflow_id, &run_id, "state").await?;

    let (answer, query_id) = state_reply(&reply)?;
    assert_eq!(answer, 1);
    assert!(!query_id.is_empty(), "handler must observe a query id");
    // Byte-identical history before and after the query: count and content.
    let after = store.read_history(&workflow_id).await?;
    assert_eq!(after, before, "the query path must never append events");

    release_and_await_42(&engine, &store, &workflow_id, &run_id).await?;
    let terminal = store.read_history(&workflow_id).await?;
    assert_eq!(
        event_kinds(&terminal),
        vec!["workflow_started", "signal_received", "workflow_completed"]
    );

    engine.shutdown()?;
    Ok(())
}

// --- test plan item 6: query after replay + determinism control ------------

#[tokio::test]
async fn recovered_workflow_answers_queries_and_matches_unqueried_control_history() -> TestResult {
    // Queried run: record progress (the "step" signal), answer one live
    // query, crash, recover, query the replayed workflow, complete.
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let first = engine_over(&store, "staged", QUERY_TIMEOUT).await?;
    let (workflow_id, run_id) = start(&first).await?;
    first
        .signal(&workflow_id, &run_id, "step", signal_payload("step")?)
        .await?;
    let (pre_crash_answer, _) =
        state_reply(&query_when_registered(&first, &workflow_id, &run_id, "state").await?)?;
    assert_eq!(pre_crash_answer, 1);
    let pre_restart = store.read_history(&workflow_id).await?;
    assert_eq!(
        event_kinds(&pre_restart),
        vec!["workflow_started", "signal_received"]
    );
    first.shutdown()?;

    let recovered = engine_over(&store, "staged", QUERY_TIMEOUT).await?;
    // Replay re-executes the fixture from the top, re-registering the
    // handler; the recovered workflow must answer with live state.
    let (answer, query_id) =
        state_reply(&query_when_registered(&recovered, &workflow_id, &run_id, "state").await?)?;
    assert_eq!(answer, 1);
    assert!(!query_id.is_empty());
    let post_recovery = store.read_history(&workflow_id).await?;
    assert_eq!(
        post_recovery, pre_restart,
        "neither replay nor queries may append or rewrite events"
    );
    release_and_await_42(&recovered, &store, &workflow_id, &run_id).await?;
    let queried_history = store.read_history(&workflow_id).await?;
    recovered.shutdown()?;

    // Control run: identical inputs and signals, never queried, no restart.
    let control_store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let control = engine_over(&control_store, "staged", QUERY_TIMEOUT).await?;
    let (control_id, control_run) = start(&control).await?;
    control
        .signal(&control_id, &control_run, "step", signal_payload("step")?)
        .await?;
    release_and_await_42(&control, &control_store, &control_id, &control_run).await?;
    let control_history = control_store.read_history(&control_id).await?;
    control.shutdown()?;

    // Determinism proof: the queried-and-recovered run's full history is
    // shape-identical to the never-queried control run's.
    assert_eq!(run_shape(&queried_history), run_shape(&control_history));
    Ok(())
}

// --- test plan item 7: suspended residency ---------------------------------

#[tokio::test]
async fn suspended_residency_query_is_not_running_without_resume_or_events() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "queryable", QUERY_TIMEOUT).await?;
    let (workflow_id, run_id) = start(&engine).await?;
    // Prove the workflow is registered and answering before suspension.
    state_reply(&query_when_registered(&engine, &workflow_id, &run_id, "state").await?)?;
    engine
        .registry()
        .replace_residency(&workflow_id, &run_id, HandleResidency::Suspended)?;
    let before = store.read_history(&workflow_id).await?;

    let error = engine
        .query(&workflow_id, &run_id, "state")
        .await
        .err()
        .ok_or("query against a suspended workflow unexpectedly succeeded")?;

    match error {
        EngineError::Query(QueryError::NotRunning(id)) => assert_eq!(id, workflow_id),
        other => return Err(format!("expected typed NotRunning, got {other:?}").into()),
    }
    // AT-007: never resume solely to answer — residency must be unchanged.
    let handle = engine
        .registry()
        .get(&workflow_id, &run_id)?
        .ok_or("suspended workflow disappeared from the registry")?;
    assert_eq!(handle.residency(), HandleResidency::Suspended);
    assert_eq!(store.read_history(&workflow_id).await?, before);

    engine.shutdown()?;
    Ok(())
}

// --- test plan item 8: unknown query name ----------------------------------

#[tokio::test]
async fn unknown_query_name_is_typed_and_workflow_still_answers() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "queryable", QUERY_TIMEOUT).await?;
    let (workflow_id, run_id) = start(&engine).await?;
    // Wait for registration first so the unknown-name error below is about
    // the name, not about registration timing.
    state_reply(&query_when_registered(&engine, &workflow_id, &run_id, "state").await?)?;
    let before = store.read_history(&workflow_id).await?;

    let error = engine
        .query(&workflow_id, &run_id, "missing")
        .await
        .err()
        .ok_or("unknown query name unexpectedly succeeded")?;

    match error {
        EngineError::Query(QueryError::UnknownQuery(name)) => assert_eq!(name, "missing"),
        other => return Err(format!("expected typed UnknownQuery, got {other:?}").into()),
    }
    assert_eq!(store.read_history(&workflow_id).await?, before);
    // The workflow was never disturbed: a follow-up valid query answers.
    let (answer, _) = state_reply(&engine.query(&workflow_id, &run_id, "state").await?)?;
    assert_eq!(answer, 1);

    release_and_await_42(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

// --- test plan item 9: raising handler --------------------------------------

#[tokio::test]
async fn raising_handler_is_handler_failed_and_workflow_survives() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "queryable", QUERY_TIMEOUT).await?;
    let (workflow_id, run_id) = start(&engine).await?;
    state_reply(&query_when_registered(&engine, &workflow_id, &run_id, "state").await?)?;
    let before = store.read_history(&workflow_id).await?;

    let error = engine
        .query(&workflow_id, &run_id, "boom")
        .await
        .err()
        .ok_or("raising handler unexpectedly produced a payload")?;

    match error {
        EngineError::Query(QueryError::HandlerFailed { message }) => {
            assert!(
                message.contains("fixture boom"),
                "failure must carry the handler's raise reason: {message}"
            );
        }
        other => return Err(format!("expected typed HandlerFailed, got {other:?}").into()),
    }
    assert_eq!(
        store.read_history(&workflow_id).await?,
        before,
        "a raising handler must append zero events"
    );

    // The workflow process survived the raise: it still answers and a
    // subsequent signal completes it normally.
    let (answer, _) = state_reply(&engine.query(&workflow_id, &run_id, "state").await?)?;
    assert_eq!(answer, 1);
    release_and_await_42(&engine, &store, &workflow_id, &run_id).await?;
    let terminal = store.read_history(&workflow_id).await?;
    assert_eq!(
        event_kinds(&terminal),
        vec!["workflow_started", "signal_received", "workflow_completed"]
    );

    engine.shutdown()?;
    Ok(())
}

// --- test plan item 10: timeout + late-reply tolerance ----------------------

#[tokio::test]
async fn unpumped_workflow_times_out_then_completes_despite_dropped_reply() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    // Short reply deadline: the fixture parks in a plain Erlang receive with
    // no pump, so the delivered query is never serviced.
    let engine = engine_over(&store, "unpumped", Duration::from_millis(200)).await?;
    let (workflow_id, run_id) = start(&engine).await?;

    let outcome = query_when_registered(&engine, &workflow_id, &run_id, "state").await;

    match outcome {
        Err(EngineError::Query(QueryError::Timeout)) => {}
        other => return Err(format!("expected typed Timeout, got {other:?}").into()),
    }
    assert_eq!(
        event_kinds(&store.read_history(&workflow_id).await?),
        vec!["workflow_started"],
        "a timed-out query must leave no trace in history"
    );

    // Late-reply tolerance: wake the raw receive (it matches the signal wake
    // marker), then let the pumped "finish" await discard the stale query
    // whose caller stopped waiting, and complete normally.
    engine
        .signal(&workflow_id, &run_id, "wake", signal_payload("wake")?)
        .await?;
    engine
        .signal(&workflow_id, &run_id, "finish", signal_payload("finish")?)
        .await?;
    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("workflow failed after query timeout: {error:?}"))?;
    assert_eq!(result_json(&result)?, json!(42));
    let terminal = store.read_history(&workflow_id).await?;
    assert_eq!(
        event_kinds(&terminal),
        vec![
            "workflow_started",
            "signal_received",
            "signal_received",
            "workflow_completed"
        ]
    );

    engine.shutdown()?;
    Ok(())
}

// --- test plan item 11: concurrent queries ----------------------------------

#[tokio::test]
async fn eight_concurrent_queries_are_all_answered_with_distinct_ids() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "queryable", QUERY_TIMEOUT).await?;
    let (workflow_id, run_id) = start(&engine).await?;
    state_reply(&query_when_registered(&engine, &workflow_id, &run_id, "state").await?)?;

    let outcomes =
        futures::future::join_all((0..8).map(|_| engine.query(&workflow_id, &run_id, "state")))
            .await;

    let mut query_ids = HashSet::new();
    for outcome in outcomes {
        let reply = outcome?;
        let (answer, query_id) = state_reply(&reply)?;
        assert_eq!(answer, 1);
        query_ids.insert(query_id);
    }
    assert_eq!(query_ids.len(), 8, "every query must get its own reply");
    // Queues drained and the pump healthy: one more query answers, and the
    // whole burst appended nothing.
    let (answer, _) = state_reply(&engine.query(&workflow_id, &run_id, "state").await?)?;
    assert_eq!(answer, 1);
    assert_eq!(
        event_kinds(&store.read_history(&workflow_id).await?),
        vec!["workflow_started"]
    );

    release_and_await_42(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

// --- test plan item 12: query racing completion ------------------------------

#[tokio::test]
async fn query_racing_completion_yields_payload_or_typed_error_without_events() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "queryable", QUERY_TIMEOUT).await?;

    for iteration in 0_u32..12 {
        let (workflow_id, run_id) = start(&engine).await?;
        // Warm up: registration finished and the workflow is parked.
        state_reply(&query_when_registered(&engine, &workflow_id, &run_id, "state").await?)?;

        // Stagger the query later and later relative to the completion
        // signal so the iterations sweep the whole race window: query
        // serviced before release, query landing mid-completion, and query
        // arriving after the run is durably terminal.
        let query_delay = Duration::from_micros(u64::from(iteration) * 300);
        let (signal_outcome, query_outcome) = tokio::join!(
            engine.signal(&workflow_id, &run_id, "release", signal_payload("release")?),
            async {
                tokio::time::sleep(query_delay).await;
                engine.query(&workflow_id, &run_id, "state").await
            },
        );

        signal_outcome?;
        match query_outcome {
            Ok(reply) => {
                let (answer, _) = state_reply(&reply)?;
                assert_eq!(answer, 1, "iteration {iteration}");
            }
            Err(EngineError::Query(QueryError::NotRunning(id))) => {
                assert_eq!(id, workflow_id, "iteration {iteration}");
            }
            Err(EngineError::Query(QueryError::ReplyDropped)) => {}
            Err(other) => {
                return Err(format!(
                    "iteration {iteration}: query racing completion must yield a payload \
                     or NotRunning/ReplyDropped, got {other:?}"
                )
                .into());
            }
        }

        let result = engine
            .result(&workflow_id, &run_id)
            .await?
            .map_err(|error| format!("iteration {iteration}: workflow failed: {error:?}"))?;
        assert_eq!(result_json(&result)?, json!(42));
        // The query path appended nothing, win or lose.
        let terminal = store.read_history(&workflow_id).await?;
        assert_eq!(
            event_kinds(&terminal),
            vec!["workflow_started", "signal_received", "workflow_completed"],
            "iteration {iteration}"
        );
        // No serviceable leftovers: a post-terminal query is typed NotRunning.
        match engine.query(&workflow_id, &run_id, "state").await {
            Err(EngineError::Query(QueryError::NotRunning(id))) => {
                assert_eq!(id, workflow_id, "iteration {iteration}");
            }
            other => {
                return Err(format!(
                    "iteration {iteration}: post-terminal query must be NotRunning, got {other:?}"
                )
                .into());
            }
        }
    }

    engine.shutdown()?;
    Ok(())
}

// --- test plan item 13: query during active execution -------------------------

#[tokio::test]
async fn query_during_active_sleep_loop_is_answered_at_the_next_yield_point() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "busy", QUERY_TIMEOUT).await?;
    let (workflow_id, run_id) = start(&engine).await?;

    // The fixture cycles 40 pumped 20ms sleeps before gating on "release",
    // so this query lands while the workflow is actively executing; the
    // engine reply deadline bounds the time to the next yield point.
    let reply = query_when_registered(&engine, &workflow_id, &run_id, "state").await?;

    let (answer, query_id) = state_reply(&reply)?;
    assert_eq!(answer, 1);
    assert!(!query_id.is_empty());

    release_and_await_42(&engine, &store, &workflow_id, &run_id).await?;
    engine.shutdown()?;
    Ok(())
}

// --- test plan item 14: Q5 servicing guard ------------------------------------

#[tokio::test]
async fn handler_calling_recording_nif_is_handler_failed_with_zero_events() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = engine_over(&store, "queryable", QUERY_TIMEOUT).await?;
    let (workflow_id, run_id) = start(&engine).await?;
    state_reply(&query_when_registered(&engine, &workflow_id, &run_id, "state").await?)?;
    let before = store.read_history(&workflow_id).await?;

    // The "records" handler calls the recording send_signal NIF while
    // servicing; the per-pid guard must refuse it typed.
    let error = engine
        .query(&workflow_id, &run_id, "records")
        .await
        .err()
        .ok_or("recording from a query handler was not refused")?;

    match error {
        EngineError::Query(QueryError::HandlerFailed { message }) => {
            assert!(
                message.contains("query_servicing") && message.contains("send_signal"),
                "failure must carry the servicing-guard refusal: {message}"
            );
        }
        other => return Err(format!("expected typed HandlerFailed, got {other:?}").into()),
    }
    assert_eq!(
        store.read_history(&workflow_id).await?,
        before,
        "the refused recording NIF must append zero events"
    );

    // The guard lifted with the failure reply: the workflow still answers
    // and completes normally.
    let (answer, _) = state_reply(&engine.query(&workflow_id, &run_id, "state").await?)?;
    assert_eq!(answer, 1);
    release_and_await_42(&engine, &store, &workflow_id, &run_id).await?;
    let terminal = store.read_history(&workflow_id).await?;
    assert_eq!(
        event_kinds(&terminal),
        vec!["workflow_started", "signal_received", "workflow_completed"]
    );

    engine.shutdown()?;
    Ok(())
}
