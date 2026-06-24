//! Durable-outbox fan-out cutover end-to-end tests (increment 3a:
//! happy path + out-of-order + duplicate dedup).
//!
//! These tests drive the **real** engine running a **real** `collect_all`
//! fan-out workflow over a **real** persistent `LibSqlStore` (which has the
//! durable `outbox` table) with `outbox.enabled` ON, and prove the cutover:
//!
//! - `dispatch_unscheduled` records fresh fan-out members through
//!   `record_fan_out_dispatch` — one atomic store transaction that writes the
//!   `ActivityScheduled`/`ActivityStarted` events AND the pending `outbox`
//!   rows — and spawns NO in-process completion task. The wired
//!   [`StubDispatcher`] asserts this: it flips a shared `fired` flag if the
//!   in-process dispatcher is ever invoked for a fresh member, and the tests
//!   assert that flag stays false — so the cutover passing on the live path
//!   instead of the outbox path fails the test loudly.
//!
//! - A worker completion is delivered out-of-band by an outbox pump that
//!   simulates the `OutboxDispatcher` + worker + completion sink: it claims
//!   pending rows from the store (`claim_outbox_rows`), and for each calls
//!   [`RuntimeHandle::deliver_outbox_completion`] — which resolves
//!   `workflow_id` → live pid via the registry, populates the runtime result
//!   map, and wakes the workflow — then marks the row done
//!   (`complete_outbox_row`). The woken workflow's `take_and_record` records
//!   each terminal through `record_fan_out_completion`, the store-backed
//!   dedup primitive, on its own single-writer Recorder.
//!
//! Every assertion is against real history read back from the store
//! (event kinds + counts), not just the workflow's return value.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{ActivityId, Event, Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_store::{EventStore, OutboxRow, OutboxStatus, OutboxStore, ReadableEventStore};
use aion_store_libsql::LibSqlStore;
use serde_json::json;

const OUTBOX_MODULE: &str = "aion_outbox_fixture";
const OUTBOX_BEAM: &[u8] = include_bytes!("fixtures/aion_outbox_fixture.beam");
const OUTBOX_SOURCE: &[u8] = include_bytes!("fixtures/aion_outbox_fixture.erl");

/// Number of fan-out members the `collect_four` fixture dispatches.
const FAN_OUT: usize = 4;
/// Generous engine reply deadline for queries (unused here but matches the
/// shared harness builder shape).
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for any polled engine-side condition.
const POLL_DEADLINE: Duration = Duration::from_secs(20);

type TestResult = Result<(), Box<dyn std::error::Error>>;

// --- the in-process dispatcher stub: must NEVER fire with the flag on ---------

/// Activity dispatcher that records the moment it is invoked into a shared
/// `fired` flag and returns an error.
///
/// With `outbox.enabled` ON the engine must NOT spawn an in-process completion
/// task for a fresh fan-out member — those members route to the durable
/// outbox. If this dispatcher ever runs, the cutover regressed (the live
/// in-process path is still firing). The flag is asserted-false after the
/// fan-out has dispatched and again at settle, so a stray invocation fails the
/// test loudly rather than letting it pass on a non-faithful path.
struct StubDispatcher {
    fired: Arc<AtomicBool>,
}

impl ActivityDispatcher for StubDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        self.fired.store(true, Ordering::SeqCst);
        Err(format!(
            "in-process activity dispatcher fired for {} (ordinal {}) — \
             outbox cutover is broken: a fresh fan-out member must route to \
             the durable outbox, not an in-process completion task",
            request.name,
            request.activity_id.sequence_position(),
        ))
    }
}

// --- harness (modeled on tests/concurrency_e2e.rs) ----------------------------

fn fixture_package(entry_function: &str) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(OUTBOX_MODULE, OUTBOX_BEAM)])?;
    let manifest = Manifest {
        entry_module: OUTBOX_MODULE.to_owned(),
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
        PackageBuilder::with_source(manifest, beams, [(OUTBOX_MODULE, OUTBOX_SOURCE.to_vec())])
            .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

/// A unique temp-file path so each test gets its own libSQL database.
fn unique_temp_path(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("aion-outbox-e2e-{name}-{pid}-{nanos}-{unique}.db"))
}

/// Build an engine over `store` (a real `LibSqlStore`) with the outbox fixture
/// loaded at `entry`, `outbox.enabled` ON, and the in-process dispatcher wired
/// to the [`StubDispatcher`] stub (which must never fire).
async fn engine_over(
    store: &Arc<LibSqlStore>,
    entry: &str,
    fired: &Arc<AtomicBool>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    let event_store: Arc<dyn EventStore> = Arc::clone(store) as Arc<dyn EventStore>;
    Ok(EngineBuilder::new()
        .store_arc(event_store)
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .query_timeout(QUERY_TIMEOUT)
        .outbox_enabled(true)
        .activity_dispatcher(Arc::new(StubDispatcher {
            fired: Arc::clone(fired),
        }))
        .load_workflows(fixture_package(entry)?)
        .build()
        .await?)
}

async fn start_collect_four(
    engine: &Engine,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            OUTBOX_MODULE,
            Payload::from_json(&json!({ "fixture": "input" }))?,
            std::collections::HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

async fn wait_for_history<F>(
    store: &Arc<LibSqlStore>,
    workflow_id: &WorkflowId,
    description: &str,
    predicate: F,
) -> Result<Vec<Event>, Box<dyn std::error::Error>>
where
    F: Fn(&[Event]) -> bool,
{
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
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

fn count_scheduled(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ActivityScheduled { .. }))
        .count()
}

fn count_completed(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ActivityCompleted { .. }))
        .count()
}

fn count_completed_for(history: &[Event], ordinal: u64) -> usize {
    history
        .iter()
        .filter(|event| match event {
            Event::ActivityCompleted { activity_id, .. } => {
                activity_id.sequence_position() == ordinal
            }
            _ => false,
        })
        .count()
}

/// The JSON-encoded worker result payload for `ordinal` (a JSON string).
fn worker_result(ordinal: u64) -> String {
    format!("\"worker-{ordinal}\"")
}

// --- shared setup: start, let it dispatch + suspend, prove the outbox path ----

/// Start a fresh `collect_four`, wait until all `FAN_OUT` members have been
/// scheduled (the run is now parked inside `collect_all`), then prove the
/// **outbox** path ran (not the live in-process path):
///
/// - exactly `FAN_OUT` `ActivityScheduled` events are recorded, with zero
///   terminals (nothing settled yet);
/// - every ordinal's outbox row is persisted `Pending` (read non-destructively
///   via `outbox_row_state`), proving `record_fan_out_dispatch` staged them.
///
/// Returns the live store + engine + workflow id + run id + the dispatcher
/// `fired` flag for the caller to drive and re-assert.
async fn started_and_suspended(
    name: &str,
) -> Result<
    (Arc<LibSqlStore>, Engine, WorkflowId, RunId, Arc<AtomicBool>),
    Box<dyn std::error::Error>,
> {
    let store = Arc::new(LibSqlStore::open(unique_temp_path(name)).await?);
    let fired = Arc::new(AtomicBool::new(false));
    let engine = engine_over(&store, "collect_four", &fired).await?;
    let (workflow_id, run_id) = start_collect_four(&engine).await?;

    // Park inside collect_all: all FAN_OUT members scheduled, none settled.
    let scheduled = wait_for_history(&store, &workflow_id, "fan-out scheduled", |events| {
        count_scheduled(events) == FAN_OUT
    })
    .await?;
    assert_eq!(
        count_completed(&scheduled),
        0,
        "no terminal may be recorded before any completion is delivered"
    );

    // PROOF the cutover ran the outbox path, not the live in-process path:
    // the in-process dispatcher never fired...
    assert!(
        !fired.load(Ordering::SeqCst),
        "in-process dispatcher must NOT fire for a fresh fan-out member under the outbox flag"
    );
    // ...and every ordinal has a persisted Pending outbox row.
    // record_fan_out_dispatch stages these atomically with the scheduling
    // events; the live in-process path would never write them.
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(&workflow_id, ordinal);
        let Some(state) = store.outbox_row_state(&key).await? else {
            return Err(format!("no outbox row staged for ordinal {ordinal} (key {key})").into());
        };
        assert_eq!(
            state.status,
            OutboxStatus::Pending,
            "outbox row for ordinal {ordinal} must be Pending after dispatch"
        );
    }

    Ok((store, engine, workflow_id, run_id, fired))
}

/// Claim every currently-pending outbox row. A claim only returns rows that
/// were `Pending`, so the returned count is itself proof of how many pending
/// rows existed.
async fn claim_all_pending(
    store: &Arc<LibSqlStore>,
) -> Result<Vec<OutboxRow>, Box<dyn std::error::Error>> {
    Ok(store.claim_outbox_rows(64).await?)
}

/// Deliver one claimed row's completion through the faithful cutover path:
/// `deliver_outbox_completion` (registry resolve → runtime map → wake) then
/// mark the outbox row done. The woken workflow records the terminal via
/// `record_fan_out_completion` on its own Recorder.
fn deliver(engine: &Engine, row: &OutboxRow) -> Result<bool, Box<dyn std::error::Error>> {
    let delivered = engine.runtime().deliver_outbox_completion(
        engine.registry(),
        &row.workflow_id,
        &ActivityId::from_sequence_position(row.ordinal),
        worker_result(row.ordinal),
    )?;
    Ok(delivered)
}

// --- case (a): happy path — deliver all N in order ----------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_happy_path_completes_with_n_terminals_in_order() -> TestResult {
    let (store, engine, workflow_id, run_id, fired) = started_and_suspended("happy").await?;

    // Claim the pending rows — exactly FAN_OUT, proving the dispatch staged
    // every member through the outbox (not the in-process path).
    let mut rows = claim_all_pending(&store).await?;
    assert_eq!(
        rows.len(),
        FAN_OUT,
        "claim must return exactly the FAN_OUT pending rows the dispatch staged"
    );
    rows.sort_by_key(|row| row.ordinal);

    // Deliver in ascending ordinal order, settling one terminal at a time so
    // the recorded order is deterministic.
    for row in &rows {
        assert!(
            deliver(&engine, row)?,
            "delivery must resolve to a live workflow pid"
        );
        store.complete_outbox_row(&row.dispatch_key).await?;
        wait_for_history(
            &store,
            &workflow_id,
            &format!("ordinal {} terminal recorded", row.ordinal),
            |events| count_completed_for(events, row.ordinal) == 1,
        )
        .await?;
    }

    // The fan-out settled: exactly FAN_OUT completions, no duplicates.
    let settled = wait_for_history(&store, &workflow_id, "fan-out settled", |events| {
        count_completed(events) == FAN_OUT
    })
    .await?;
    assert_eq!(count_completed(&settled), FAN_OUT);
    for ordinal in 0..FAN_OUT as u64 {
        assert_eq!(
            count_completed_for(&settled, ordinal),
            1,
            "ordinal {ordinal} must have exactly one terminal"
        );
    }
    // The in-process dispatcher never fired across the whole run.
    assert!(
        !fired.load(Ordering::SeqCst),
        "in-process dispatcher must never fire under the outbox flag"
    );

    // The workflow returns the collected results (collect_all returns the
    // per-ordinal result payload text, in input order).
    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("collect_four failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(
        value,
        json!([
            worker_result(0),
            worker_result(1),
            worker_result(2),
            worker_result(3),
        ]),
        "collect_all must return all {FAN_OUT} results in input order"
    );

    engine.shutdown()?;
    Ok(())
}

// --- case (b): out-of-order — deliver in REVERSE ordinal order ----------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_out_of_order_completions_still_settle_with_n_terminals() -> TestResult {
    let (store, engine, workflow_id, run_id, fired) = started_and_suspended("reverse").await?;

    let mut rows = claim_all_pending(&store).await?;
    assert_eq!(rows.len(), FAN_OUT);
    // Deliver in DESCENDING ordinal order.
    rows.sort_by_key(|row| std::cmp::Reverse(row.ordinal));

    for row in &rows {
        assert!(deliver(&engine, row)?);
        store.complete_outbox_row(&row.dispatch_key).await?;
        wait_for_history(
            &store,
            &workflow_id,
            &format!("ordinal {} terminal recorded", row.ordinal),
            |events| count_completed_for(events, row.ordinal) == 1,
        )
        .await?;
    }

    let settled = wait_for_history(&store, &workflow_id, "fan-out settled", |events| {
        count_completed(events) == FAN_OUT
    })
    .await?;
    for ordinal in 0..FAN_OUT as u64 {
        assert_eq!(
            count_completed_for(&settled, ordinal),
            1,
            "ordinal {ordinal} must have exactly one terminal regardless of delivery order"
        );
    }
    assert!(
        !fired.load(Ordering::SeqCst),
        "in-process dispatcher must never fire under the outbox flag"
    );

    // Despite reverse delivery, the result list is input-ordered.
    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("collect_four failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(
        value,
        json!([
            worker_result(0),
            worker_result(1),
            worker_result(2),
            worker_result(3),
        ]),
        "collect_all is input-ordered even when completions arrive reversed"
    );

    engine.shutdown()?;
    Ok(())
}

// --- case (c): duplicate completion — deliver one ordinal TWICE ---------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_duplicate_completion_records_exactly_one_terminal() -> TestResult {
    let (store, engine, workflow_id, run_id, fired) = started_and_suspended("dup").await?;

    let mut rows = claim_all_pending(&store).await?;
    assert_eq!(rows.len(), FAN_OUT);
    rows.sort_by_key(|row| row.ordinal);

    // The ordinal we will deliver TWICE.
    let dup_ordinal = 0u64;
    let Some(dup_row) = rows.iter().find(|row| row.ordinal == dup_ordinal).cloned() else {
        return Err("no claimed row for the duplicated ordinal".into());
    };

    // First delivery of the duplicated ordinal: records exactly one terminal.
    assert!(deliver(&engine, &dup_row)?);
    store.complete_outbox_row(&dup_row.dispatch_key).await?;
    wait_for_history(
        &store,
        &workflow_id,
        "first delivery of duplicated ordinal recorded",
        |events| count_completed_for(events, dup_ordinal) == 1,
    )
    .await?;

    // SECOND delivery of the SAME ordinal: re-route the same completion. The
    // dedup (record_fan_out_completion finding the ordinal already resolved,
    // plus settle_all's recorded-terminal short-circuit) must hold — no second
    // terminal, no double-complete, no panic.
    assert!(
        deliver(&engine, &dup_row)?,
        "redelivery still resolves to the live pid"
    );
    // Give the woken workflow time to process (and drop) the duplicate.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let after_dup = store.read_history(&workflow_id).await?;
    assert_eq!(
        count_completed_for(&after_dup, dup_ordinal),
        1,
        "a duplicate completion must NOT record a second terminal for ordinal {dup_ordinal}"
    );

    // Deliver the remaining ordinals once each so the fan-out can settle.
    for row in rows.iter().filter(|row| row.ordinal != dup_ordinal) {
        assert!(deliver(&engine, row)?);
        store.complete_outbox_row(&row.dispatch_key).await?;
        wait_for_history(
            &store,
            &workflow_id,
            &format!("ordinal {} terminal recorded", row.ordinal),
            |events| count_completed_for(events, row.ordinal) == 1,
        )
        .await?;
    }

    let settled = wait_for_history(&store, &workflow_id, "fan-out settled", |events| {
        count_completed(events) == FAN_OUT
    })
    .await?;
    // Exactly FAN_OUT terminals total — the duplicate added nothing.
    assert_eq!(
        count_completed(&settled),
        FAN_OUT,
        "the duplicate must not inflate the terminal count"
    );
    for ordinal in 0..FAN_OUT as u64 {
        assert_eq!(
            count_completed_for(&settled, ordinal),
            1,
            "every ordinal (including the duplicated one) has exactly one terminal"
        );
    }

    // The workflow completed exactly once with the full result list.
    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("collect_four failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(
        value,
        json!([
            worker_result(0),
            worker_result(1),
            worker_result(2),
            worker_result(3),
        ]),
    );
    let completed_terminals = settled
        .iter()
        .filter(|event| matches!(event, Event::WorkflowCompleted { .. }))
        .count();
    assert_eq!(
        completed_terminals, 1,
        "the workflow must complete exactly once despite the duplicate completion"
    );
    assert!(
        !fired.load(Ordering::SeqCst),
        "in-process dispatcher must never fire under the outbox flag"
    );

    engine.shutdown()?;
    Ok(())
}
