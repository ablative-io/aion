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
use aion_core::{ActivityErrorKind, ActivityId, Event, Payload, RunId, WorkflowId};
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

async fn start_fixture_workflow(
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

/// Count `ActivityFailed` events across `history` (mirror of `count_completed`).
fn count_failed(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ActivityFailed { .. }))
        .count()
}

/// Count `ActivityFailed` events for a single `ordinal`.
fn count_failed_for(history: &[Event], ordinal: u64) -> usize {
    history
        .iter()
        .filter(|event| match event {
            Event::ActivityFailed { activity_id, .. } => activity_id.sequence_position() == ordinal,
            _ => false,
        })
        .count()
}

/// Count `ActivityCancelled` events across `history` (mirror of `count_completed`).
fn count_cancelled(history: &[Event]) -> usize {
    history
        .iter()
        .filter(|event| matches!(event, Event::ActivityCancelled { .. }))
        .count()
}

/// Count `ActivityCancelled` events for a single `ordinal`.
fn count_cancelled_for(history: &[Event], ordinal: u64) -> usize {
    history
        .iter()
        .filter(|event| match event {
            Event::ActivityCancelled { activity_id, .. } => {
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

/// Start a fresh fan-out workflow at `entry` (a four-member `collect_*`),
/// wait until all `FAN_OUT` members have been scheduled (the run is now
/// parked inside the collect native), then prove the **outbox** path ran
/// (not the live in-process path):
///
/// - exactly `FAN_OUT` `ActivityScheduled` events are recorded, with zero
///   terminals (nothing settled yet);
/// - every ordinal's outbox row is persisted `Pending` (read non-destructively
///   via `outbox_row_state`), proving `record_fan_out_dispatch` staged them.
///
/// The dispatch+suspend proof is shape-agnostic — `collect_all`,
/// `collect_race` and `collect_map` all stage their four fresh members
/// through the identical `dispatch_unscheduled` → `record_fan_out_dispatch`
/// path before any settlement rule runs — so every settle shape shares this
/// setup. Returns the live store + engine + workflow id + run id + the
/// dispatcher `fired` flag for the caller to drive and re-assert.
async fn started_and_suspended_at(
    name: &str,
    entry: &str,
) -> Result<
    (Arc<LibSqlStore>, Engine, WorkflowId, RunId, Arc<AtomicBool>),
    Box<dyn std::error::Error>,
> {
    let store = Arc::new(LibSqlStore::open(unique_temp_path(name)).await?);
    let fired = Arc::new(AtomicBool::new(false));
    let engine = engine_over(&store, entry, &fired).await?;
    let (workflow_id, run_id) = start_fixture_workflow(&engine).await?;

    // Park inside the collect native: all FAN_OUT members scheduled, none settled.
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

/// `started_and_suspended_at` specialized to the `collect_four` (`collect_all`)
/// entry — the existing happy-path/out-of-order/duplicate/crash/fail-fast cases.
async fn started_and_suspended(
    name: &str,
) -> Result<
    (Arc<LibSqlStore>, Engine, WorkflowId, RunId, Arc<AtomicBool>),
    Box<dyn std::error::Error>,
> {
    started_and_suspended_at(name, "collect_four").await
}

/// Claim every currently-pending outbox row. A claim only returns rows that
/// were `Pending`, so the returned count is itself proof of how many pending
/// rows existed.
async fn claim_all_pending(
    store: &Arc<LibSqlStore>,
) -> Result<Vec<OutboxRow>, Box<dyn std::error::Error>> {
    Ok(store.claim_outbox_rows(64).await?)
}

/// Poll until every `ordinal` in `ordinals` has its outbox row back at
/// `Pending` (the state the crash-recovery re-arm restores), or time out.
async fn wait_for_rows_pending(
    store: &Arc<LibSqlStore>,
    workflow_id: &WorkflowId,
    ordinals: &[u64],
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let mut all_pending = true;
        for &ordinal in ordinals {
            let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
            let status = store
                .outbox_row_state(&key)
                .await?
                .map(|state| state.status);
            if status != Some(OutboxStatus::Pending) {
                all_pending = false;
                break;
            }
        }
        if all_pending {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            let mut states = Vec::new();
            for &ordinal in ordinals {
                let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
                states.push((ordinal, store.outbox_row_state(&key).await?));
            }
            return Err(format!(
                "timed out waiting for re-arm to flip rows back to Pending: {states:?}"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Assert the pre-crash stranded state: ordinals 0,1 have exactly one terminal
/// each, ordinals 2,3 have none, the 2,3 outbox rows are `Done`, and a fresh
/// claim returns nothing (a `Done` row is not claimable — so without the
/// recovery re-arm the LOST ordinals are stranded forever).
async fn assert_lost_rows_stranded(
    store: &Arc<LibSqlStore>,
    workflow_id: &WorkflowId,
) -> Result<(), Box<dyn std::error::Error>> {
    let pre_crash = store.read_history(workflow_id).await?;
    assert_eq!(
        count_completed(&pre_crash),
        2,
        "only the two RECORDED ordinals have terminals before the crash"
    );
    assert_eq!(count_completed_for(&pre_crash, 0), 1);
    assert_eq!(count_completed_for(&pre_crash, 1), 1);
    assert_eq!(
        count_completed_for(&pre_crash, 2),
        0,
        "LOST ordinal 2 has NO terminal before the crash"
    );
    assert_eq!(
        count_completed_for(&pre_crash, 3),
        0,
        "LOST ordinal 3 has NO terminal before the crash"
    );
    for ordinal in [2u64, 3u64] {
        let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
        let state = store
            .outbox_row_state(&key)
            .await?
            .ok_or_else(|| format!("no outbox row for LOST ordinal {ordinal}"))?;
        assert_eq!(
            state.status,
            OutboxStatus::Done,
            "LOST ordinal {ordinal} row must be Done (worker accepted) before the crash"
        );
    }
    let nothing = store.claim_outbox_rows(64).await?;
    assert!(
        nothing.is_empty(),
        "Done rows are not claimable — the LOST ordinals are stranded without re-arm, \
         got {nothing:?}"
    );
    Ok(())
}

/// Assert the post-recovery settled state: exactly `FAN_OUT` terminals, one per
/// ordinal (no duplicate across the crash), and exactly one `WorkflowCompleted`.
async fn assert_settled_no_duplicates(
    store: &Arc<LibSqlStore>,
    workflow_id: &WorkflowId,
) -> Result<(), Box<dyn std::error::Error>> {
    let settled = wait_for_history(store, workflow_id, "fan-out settled", |events| {
        count_completed(events) == FAN_OUT
    })
    .await?;
    assert_eq!(
        count_completed(&settled),
        FAN_OUT,
        "exactly FAN_OUT terminals after recovery"
    );
    for ordinal in 0..FAN_OUT as u64 {
        assert_eq!(
            count_completed_for(&settled, ordinal),
            1,
            "ordinal {ordinal} has exactly one terminal — no duplicate across the crash"
        );
    }
    let completed_terminals = settled
        .iter()
        .filter(|event| matches!(event, Event::WorkflowCompleted { .. }))
        .count();
    assert_eq!(
        completed_terminals, 1,
        "the workflow completes exactly once across the crash boundary"
    );
    Ok(())
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

/// Failure twin of [`deliver`]: route one claimed row's terminal FAILURE
/// through the faithful cutover path
/// ([`RuntimeHandle::deliver_outbox_failure`]: registry resolve → runtime
/// error map → wake). The woken workflow's `take_and_record` records the
/// terminal through `record_fan_out_completion(FanOutOutcome::Failed{..})` on
/// its own Recorder, and `settle_all` fails fast on it. `reason` becomes the
/// recorded `ActivityFailed`'s `error.message` (classified Terminal).
fn deliver_failure(
    engine: &Engine,
    row: &OutboxRow,
    reason: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let delivered = engine.runtime().deliver_outbox_failure(
        engine.registry(),
        &row.workflow_id,
        &ActivityId::from_sequence_position(row.ordinal),
        reason.to_owned(),
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

// --- case (d): crash mid-flight — recovery re-arms a stranded "done" row ------

/// The crash-recovery re-arm (increment 3b-i) closes the lost-completion hole.
///
/// Scenario: a fan-out is dispatched and four outbox rows are staged. Two
/// ordinals (RECORDED = {0,1}) complete normally — terminal recorded, row
/// `Done`. The other two (LOST = {2,3}) are marked `Done` by the
/// `OutboxDispatcher` the instant the worker *accepts* them, but the actual
/// completion is lost in a crash before any terminal is recorded. A `Done`
/// outbox row is NOT claimable, so without re-arm those two ordinals are
/// stranded forever: the workflow has no terminal for them and the dispatcher
/// will never re-deliver — the workflow can never finish.
///
/// On recovery a fresh engine over the SAME store replays the parked run. The
/// first arrival into `collect_all` sees ordinals 2,3 are scheduled-but-have-no
/// -terminal (stale) and re-arms their durable outbox rows back to claimable
/// `Pending` via `rearm_outbox_pending` — it does NOT spawn the in-process
/// completion dispatcher (the `fired2` flag proves this). The re-armed rows are
/// then claimed + delivered like any other, and the fan-out settles with
/// exactly one terminal per ordinal across the crash boundary.
///
/// Faithfulness anchors:
/// - the LOST rows reach `Done`-without-terminal BEFORE the crash (step 5),
///   so the test genuinely exercises re-arm rescuing a stranded row;
/// - the restart is a real second engine over the same `Arc<LibSqlStore>` that
///   recovers/replays the run (modeled on `concurrency_e2e.rs`
///   `restart_replay_and_finish`), not a re-`start_workflow` or recorder poke;
/// - recovery must re-arm via the outbox, NOT drive the in-process dispatcher
///   (`fired2` stays false) — that is what makes the rescue durable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_crash_recovery_rearms_stranded_done_row_and_finishes() -> TestResult {
    let (store, engine1, workflow_id, run_id, fired1) = started_and_suspended("restart").await?;

    // Claim all four staged rows (Pending -> Claimed). Split into the two we
    // will faithfully complete and the two whose completion we will "lose".
    let mut rows = claim_all_pending(&store).await?;
    assert_eq!(
        rows.len(),
        FAN_OUT,
        "claim must return exactly the FAN_OUT pending rows the dispatch staged"
    );
    rows.sort_by_key(|row| row.ordinal);
    let recorded: Vec<OutboxRow> = rows.iter().filter(|r| r.ordinal < 2).cloned().collect();
    let lost: Vec<OutboxRow> = rows.iter().filter(|r| r.ordinal >= 2).cloned().collect();
    assert_eq!(recorded.len(), 2, "RECORDED = ordinals {{0,1}}");
    assert_eq!(lost.len(), 2, "LOST = ordinals {{2,3}}");

    // RECORDED {0,1}: deliver the completion (terminal recorded) then mark the
    // outbox row Done — the normal, fully-acknowledged path.
    for row in &recorded {
        assert!(
            deliver(&engine1, row)?,
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

    // LOST {2,3}: mark the row Done WITHOUT delivering. This simulates the
    // OutboxDispatcher flipping the row to `done` the moment the worker accepts
    // the activity, then crashing before the completion reaches the workflow —
    // the terminal is never recorded.
    for row in &lost {
        store.complete_outbox_row(&row.dispatch_key).await?;
    }

    // --- pre-crash state: the LOST rows are stranded ------------------------
    assert_lost_rows_stranded(&store, &workflow_id).await?;
    assert!(
        !fired1.load(Ordering::SeqCst),
        "in-process dispatcher must not have fired before the crash"
    );

    // --- CRASH: shut the engine down and drop it ---------------------------
    engine1.shutdown()?;
    drop(engine1);

    // --- RECOVER: a fresh engine over the SAME store replays the run -------
    let fired2 = Arc::new(AtomicBool::new(false));
    let engine2 = engine_over(&store, "collect_four", &fired2).await?;

    // Recovery replays the parked run: ordinals 0,1 resolve from history;
    // ordinals 2,3 are scheduled-no-terminal => stale => re-armed to claimable
    // `Pending` on the first arrival into collect_all. Poll until both LOST
    // rows are Pending again.
    wait_for_rows_pending(&store, &workflow_id, &[2, 3]).await?;

    // KEY FAITHFULNESS ASSERTION: recovery re-armed via the outbox, it did NOT
    // drive the in-process dispatcher. Without 3b-i these rows stay Done forever.
    assert!(
        !fired2.load(Ordering::SeqCst),
        "recovery must re-arm the stranded rows via the outbox, NOT drive the \
         in-process dispatcher"
    );

    // --- pump the re-armed rows like any other Pending dispatch ------------
    let mut revived = claim_all_pending(&store).await?;
    assert_eq!(
        revived.len(),
        2,
        "the re-arm makes exactly the two LOST ordinals claimable again"
    );
    revived.sort_by_key(|row| row.ordinal);
    assert_eq!(
        revived.iter().map(|r| r.ordinal).collect::<Vec<_>>(),
        vec![2, 3],
        "the claimable rows are exactly the LOST ordinals"
    );
    for row in &revived {
        assert!(
            deliver(&engine2, row)?,
            "re-delivery must resolve to the recovered workflow pid"
        );
        store.complete_outbox_row(&row.dispatch_key).await?;
        wait_for_history(
            &store,
            &workflow_id,
            &format!("re-armed ordinal {} terminal recorded", row.ordinal),
            |events| count_completed_for(events, row.ordinal) == 1,
        )
        .await?;
    }

    // --- final state: settled across the crash, no duplicates -------------
    assert_settled_no_duplicates(&store, &workflow_id).await?;

    // The recovered workflow returns the full input-ordered result list.
    let result = engine2
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("collect_four failed after recovery: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(
        value,
        json!([
            worker_result(0),
            worker_result(1),
            worker_result(2),
            worker_result(3),
        ]),
        "collect_all returns all {FAN_OUT} results in input order after recovery"
    );

    // Recovery never touched the in-process dispatcher.
    assert!(
        !fired2.load(Ordering::SeqCst),
        "the recovered engine must never fire the in-process dispatcher"
    );

    engine2.shutdown()?;
    Ok(())
}

// --- case (e): fail-fast — one member fails, siblings are cancelled -----------

/// A terminal FAILURE delivered for one fan-out member makes `collect_all`
/// fail fast: it records that member's `ActivityFailed`, cancels every
/// unresolved sibling (`ActivityCancelled`), and propagates the failure out of
/// the workflow.
///
/// This exercises the failure twin of the happy path end to end:
/// `deliver_outbox_failure` (registry resolve → runtime error map → wake) →
/// `take_and_record` → `record_fan_out_completion(FanOutOutcome::Failed{..})`
/// for the failing ordinal, then `settle_all`'s fail-fast branch records
/// `ActivityCancelled` for the three still-`Pending` siblings via
/// `cancel_pending`.
///
/// Observed real fail-fast event shape (asserted below): exactly ONE
/// `ActivityFailed` for the delivered ordinal (carrying the delivered reason,
/// classified Terminal, attempt 1) and exactly THREE `ActivityCancelled`, one
/// per unresolved sibling — `FAN_OUT` terminals total, one per ordinal, no
/// `ActivityCompleted`. The fixture's `{ok, Results} = collect_all(..)` match
/// fails on the propagated `{error, _}`, so the workflow does not complete
/// successfully and `engine.result(..)` surfaces an `Err`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_failure_fails_fast_and_cancels_unresolved_siblings() -> TestResult {
    let (store, engine, workflow_id, run_id, fired) = started_and_suspended("failfast").await?;

    let mut rows = claim_all_pending(&store).await?;
    assert_eq!(
        rows.len(),
        FAN_OUT,
        "claim must return exactly the FAN_OUT pending rows the dispatch staged"
    );
    rows.sort_by_key(|row| row.ordinal);

    // The single ordinal we deliver as a terminal FAILURE.
    let failed_ordinal = 1u64;
    let reason = "terminal:boom";
    let Some(failed_row) = rows
        .iter()
        .find(|row| row.ordinal == failed_ordinal)
        .cloned()
    else {
        return Err("no claimed row for the failed ordinal".into());
    };

    // Deliver ONE member as a failure (the other three stay Pending). Mark its
    // outbox row done — the worker reported a terminal failure, the row is
    // acknowledged just like a success.
    assert!(
        deliver_failure(&engine, &failed_row, reason)?,
        "failure delivery must resolve to a live workflow pid"
    );
    store.complete_outbox_row(&failed_row.dispatch_key).await?;

    // Fail-fast settles the whole batch in one sweep: one Failed (ordinal 1) +
    // three Cancelled (ordinals 0,2,3). Wait until all FAN_OUT terminals land.
    let settled = wait_for_history(&store, &workflow_id, "fail-fast settled", |events| {
        count_failed(events) + count_cancelled(events) == FAN_OUT
    })
    .await?;

    // The failing ordinal: exactly one ActivityFailed, no other terminal kind.
    assert_eq!(
        count_failed(&settled),
        1,
        "exactly one ActivityFailed across the batch: {settled:#?}"
    );
    assert_eq!(
        count_failed_for(&settled, failed_ordinal),
        1,
        "ordinal {failed_ordinal} has exactly one ActivityFailed"
    );

    // The recorded ActivityFailed carries the delivered reason faithfully:
    // classified Terminal, attempt 1 (test 3 folded in — deliver_outbox_failure
    // → ActivityFailed shape).
    let Some(Event::ActivityFailed { error, attempt, .. }) = settled.iter().find(|event| {
        matches!(
            event,
            Event::ActivityFailed { activity_id, .. }
                if activity_id.sequence_position() == failed_ordinal
        )
    }) else {
        return Err(format!(
            "expected an ActivityFailed for ordinal {failed_ordinal}: {settled:#?}"
        )
        .into());
    };
    assert_eq!(
        error.message, reason,
        "the recorded failure carries the reason deliver_outbox_failure passed"
    );
    assert_eq!(
        error.kind,
        ActivityErrorKind::Terminal,
        "a delivered terminal failure is classified Terminal"
    );
    assert_eq!(*attempt, 1, "first (and only) delivery attempt");

    // The three unresolved siblings are each recorded ActivityCancelled by the
    // fail-fast cancellation (cancel_pending), exactly once each.
    assert_eq!(
        count_cancelled(&settled),
        FAN_OUT - 1,
        "every unresolved sibling is cancelled: {settled:#?}"
    );
    for ordinal in [0u64, 2, 3] {
        assert_eq!(
            count_cancelled_for(&settled, ordinal),
            1,
            "sibling ordinal {ordinal} is cancelled exactly once"
        );
    }

    // No member completed, and exactly one terminal per ordinal (one Failed +
    // three Cancelled) — no duplicates.
    assert_eq!(
        count_completed(&settled),
        0,
        "fail-fast records no ActivityCompleted"
    );
    for ordinal in 0..FAN_OUT as u64 {
        let terminals = count_completed_for(&settled, ordinal)
            + count_failed_for(&settled, ordinal)
            + count_cancelled_for(&settled, ordinal);
        assert_eq!(
            terminals, 1,
            "ordinal {ordinal} has exactly one terminal (Failed or Cancelled), no duplicates"
        );
    }

    // The failure propagates out of the workflow: the fixture's `{ok, _}` match
    // fails on the `{error, _}` collect result, so the run does not complete
    // successfully and the result is an Err.
    let outcome = engine.result(&workflow_id, &run_id).await?;
    assert!(
        outcome.is_err(),
        "collect_all fail-fast must propagate the failure out of the workflow, got {outcome:?}"
    );

    // The in-process dispatcher never fired across the whole run.
    assert!(
        !fired.load(Ordering::SeqCst),
        "in-process dispatcher must never fire under the outbox flag"
    );

    engine.shutdown()?;
    Ok(())
}

// --- case (f): late completion for a cancelled ordinal is dropped -------------

/// A LATE success completion arriving for an ordinal that fail-fast already
/// recorded `ActivityCancelled` is DROPPED — the cancellation stands, NO
/// `ActivityCompleted` is appended, and the workflow outcome does not change.
///
/// This proves the dedup contract end to end: `record_fan_out_completion`'s
/// `ordinal_is_resolved` predicate treats `ActivityCancelled` as a terminal
/// resolution, so a worker completion that lands after the cancellation is a
/// no-op rather than a recorded-over terminal.
///
/// Faithfulness note on the observed semantics: fail-fast removes the pending
/// await and the fixture's `{ok, _}` match fails on the propagated `{error,_}`,
/// so the workflow process exits — after fail-fast the run is no longer live.
/// A late `deliver` therefore cannot reach a live mailbox: the delivery is
/// rejected (the resolved process is no longer live) and never reaches the
/// recorder, and the retained-completion drain on process exit clears any
/// racing entry. The exact rejection form (a not-live `Ok(false)`, or an
/// `Err` because the process slot is already gone) is incidental — both mean
/// the completion was never recorded. What this test pins is the durable
/// outcome: the `ActivityCancelled` terminal for the ordinal still stands and
/// NO `ActivityCompleted` was ever appended for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_late_completion_for_cancelled_ordinal_is_dropped() -> TestResult {
    // Reuse the fail-fast scenario: deliver ordinal 1 as a failure so the
    // siblings (0,2,3) are cancelled.
    let (store, engine, workflow_id, run_id, _fired) = started_and_suspended("latecancel").await?;

    let mut rows = claim_all_pending(&store).await?;
    assert_eq!(rows.len(), FAN_OUT);
    rows.sort_by_key(|row| row.ordinal);

    let failed_ordinal = 1u64;
    // The cancelled sibling we will then try to complete late.
    let cancelled_ordinal = 0u64;
    let Some(failed_row) = rows
        .iter()
        .find(|row| row.ordinal == failed_ordinal)
        .cloned()
    else {
        return Err("no claimed row for the failed ordinal".into());
    };
    let Some(cancelled_row) = rows
        .iter()
        .find(|row| row.ordinal == cancelled_ordinal)
        .cloned()
    else {
        return Err("no claimed row for the cancelled ordinal".into());
    };

    assert!(deliver_failure(&engine, &failed_row, "terminal:boom")?);
    store.complete_outbox_row(&failed_row.dispatch_key).await?;

    // Wait until the sibling has been recorded ActivityCancelled by fail-fast.
    let cancelled = wait_for_history(
        &store,
        &workflow_id,
        &format!("ordinal {cancelled_ordinal} cancelled"),
        |events| count_cancelled_for(events, cancelled_ordinal) == 1,
    )
    .await?;
    assert_eq!(
        count_cancelled_for(&cancelled, cancelled_ordinal),
        1,
        "the sibling was cancelled by fail-fast"
    );
    assert_eq!(
        count_completed_for(&cancelled, cancelled_ordinal),
        0,
        "no completion yet for the cancelled ordinal"
    );

    // Deliver a LATE success completion for the SAME cancelled ordinal. The
    // cancellation already terminally resolved the ordinal: this completion can
    // never become a recorded terminal over the ActivityCancelled. After
    // fail-fast the run has exited, so the delivery is rejected before reaching
    // any mailbox (no live pid / process gone) — we tolerate either rejection
    // form; the assertions below pin the durable outcome regardless.
    let delivered = deliver(&engine, &cancelled_row);
    assert!(
        !matches!(delivered, Ok(true)),
        "a late completion for a cancelled ordinal on a fail-fast-terminated run \
         must NOT deliver to a live workflow, got {delivered:?}"
    );
    store
        .complete_outbox_row(&cancelled_row.dispatch_key)
        .await?;

    // Give any woken processing time to (not) record the late completion.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The cancellation stands and NO ActivityCompleted was appended.
    let after_late = store.read_history(&workflow_id).await?;
    assert_eq!(
        count_cancelled_for(&after_late, cancelled_ordinal),
        1,
        "the ActivityCancelled terminal for ordinal {cancelled_ordinal} still stands"
    );
    assert_eq!(
        count_completed_for(&after_late, cancelled_ordinal),
        0,
        "a late completion for a CANCELLED ordinal must be dropped, not recorded"
    );
    // The ordinal still has exactly one terminal — the cancellation — with no
    // duplicate or second-terminal collision.
    let terminals = count_completed_for(&after_late, cancelled_ordinal)
        + count_failed_for(&after_late, cancelled_ordinal)
        + count_cancelled_for(&after_late, cancelled_ordinal);
    assert_eq!(
        terminals, 1,
        "the cancelled ordinal still has exactly one terminal (the cancellation)"
    );

    // The workflow outcome is unchanged by the late completion: still a
    // propagated failure (no panic, no flip to success).
    let outcome = engine.result(&workflow_id, &run_id).await?;
    assert!(
        outcome.is_err(),
        "a late completion for a cancelled ordinal must not flip the failed outcome, got {outcome:?}"
    );

    engine.shutdown()?;
    Ok(())
}

// --- case (g): collect_race under the flag — winner settles, losers cancelled -

/// Confirmation coverage that a four-member `collect_race` settles end to end
/// through the SAME shape-agnostic outbox cutover as `collect_all`.
///
/// `collect_race` dispatches its four fresh members through the identical
/// `dispatch_unscheduled` → `record_fan_out_dispatch` path (proven by the
/// shared [`started_and_suspended_at`] setup: four `Pending` outbox rows, the
/// in-process dispatcher never fired). We then deliver exactly ONE member's
/// completion via the faithful cutover path ([`deliver`] →
/// `deliver_outbox_completion` → `take_and_record` →
/// `record_fan_out_completion`), and `settle_race` settles the batch.
///
/// Observed real `settle_race` winner/loser event shape (asserted below): the
/// single delivered ordinal is recorded `ActivityCompleted` (exactly ONE
/// `ActivityCompleted` across the batch), and the three unresolved siblings are
/// recorded `ActivityCancelled` (one each) by the loser-cancellation sweep —
/// `FAN_OUT` terminals total, one per ordinal, no `ActivityFailed`. The fixture
/// returns the winner's payload, so `engine.result` is the winner's worker
/// result. The in-process dispatcher never fires across the whole run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_race_settles_first_completion_and_cancels_losers() -> TestResult {
    let (store, engine, workflow_id, run_id, fired) =
        started_and_suspended_at("race", "collect_race_four").await?;

    // Claim the four staged rows — exactly FAN_OUT, proving the race dispatch
    // staged every member through the outbox, not the in-process path.
    let mut rows = claim_all_pending(&store).await?;
    assert_eq!(
        rows.len(),
        FAN_OUT,
        "claim must return exactly the FAN_OUT pending rows the race dispatch staged"
    );
    rows.sort_by_key(|row| row.ordinal);

    // Deliver exactly ONE member's completion — the race winner. The other
    // three stay Pending and must be cancelled by settle_race.
    let winner_ordinal = 2u64;
    let Some(winner_row) = rows
        .iter()
        .find(|row| row.ordinal == winner_ordinal)
        .cloned()
    else {
        return Err("no claimed row for the winner ordinal".into());
    };
    assert!(
        deliver(&engine, &winner_row)?,
        "winner delivery must resolve to a live workflow pid"
    );
    store.complete_outbox_row(&winner_row.dispatch_key).await?;

    // settle_race resolves in one sweep: the delivered ordinal becomes the
    // recorded winner (ActivityCompleted) and the three unresolved siblings are
    // cancelled (ActivityCancelled). Wait until all FAN_OUT terminals land.
    let settled = wait_for_history(&store, &workflow_id, "race settled", |events| {
        count_completed(events) + count_cancelled(events) == FAN_OUT
    })
    .await?;

    // The winner: exactly one ActivityCompleted across the whole batch, for the
    // delivered ordinal, and no ActivityFailed anywhere.
    assert_eq!(
        count_completed(&settled),
        1,
        "exactly one ActivityCompleted (the race winner): {settled:#?}"
    );
    assert_eq!(
        count_completed_for(&settled, winner_ordinal),
        1,
        "ordinal {winner_ordinal} is the recorded winner"
    );
    assert_eq!(
        count_failed(&settled),
        0,
        "a successful winner records no ActivityFailed"
    );

    // The three unresolved siblings are each recorded ActivityCancelled exactly
    // once by settle_race's loser-cancellation.
    assert_eq!(
        count_cancelled(&settled),
        FAN_OUT - 1,
        "every losing sibling is cancelled: {settled:#?}"
    );
    for ordinal in [0u64, 1, 3] {
        assert_eq!(
            count_cancelled_for(&settled, ordinal),
            1,
            "losing sibling ordinal {ordinal} is cancelled exactly once"
        );
    }

    // Exactly one terminal per ordinal (one Completed + three Cancelled), no
    // duplicates.
    for ordinal in 0..FAN_OUT as u64 {
        let terminals = count_completed_for(&settled, ordinal)
            + count_failed_for(&settled, ordinal)
            + count_cancelled_for(&settled, ordinal);
        assert_eq!(
            terminals, 1,
            "ordinal {ordinal} has exactly one terminal (Completed or Cancelled), no duplicates"
        );
    }

    // The workflow settles with the delivered member as the winner: the fixture
    // returns the winner's payload, so engine.result is that worker result.
    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("collect_race_four failed: {error:?}"))?;
    // The winner's payload is the single JSON-string worker result itself (NOT
    // wrapped in a list as collect_all/collect_map are): the recorded
    // ActivityCompleted payload `"worker-N"` is returned verbatim, so it
    // deserializes to the JSON string value `worker-N`.
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(
        value,
        json!(format!("worker-{winner_ordinal}")),
        "collect_race returns the winner's (ordinal {winner_ordinal}) result verbatim"
    );

    // The race completed exactly once.
    let completed_terminals = settled
        .iter()
        .filter(|event| matches!(event, Event::WorkflowCompleted { .. }))
        .count();
    assert_eq!(
        completed_terminals, 1,
        "the race workflow completes exactly once"
    );

    // The in-process dispatcher never fired across the whole run.
    assert!(
        !fired.load(Ordering::SeqCst),
        "in-process dispatcher must never fire under the outbox flag"
    );

    engine.shutdown()?;
    Ok(())
}

// --- case (h): collect_map under the flag — all N complete, ordered result ----

/// Confirmation coverage that a four-member `collect_map` settles end to end
/// through the SAME shape-agnostic outbox cutover.
///
/// `collect_map` is the `CollectKind::All` settlement (`settle_all`) reached
/// through the distinct `collect_map` NIF entrypoint: every member must
/// complete and the result is the per-ordinal payloads in input order. This
/// test drives the distinct NIF symbol through the outbox path to confirm it
/// routes identically to `collect_all` — all four members delivered via the
/// outbox, `FAN_OUT` `ActivityCompleted`, one `WorkflowCompleted`, and the
/// input-ordered collected list as the result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_map_completes_with_n_terminals_and_ordered_result() -> TestResult {
    let (store, engine, workflow_id, run_id, fired) =
        started_and_suspended_at("map", "collect_map_four").await?;

    let mut rows = claim_all_pending(&store).await?;
    assert_eq!(
        rows.len(),
        FAN_OUT,
        "claim must return exactly the FAN_OUT pending rows the map dispatch staged"
    );
    rows.sort_by_key(|row| row.ordinal);

    // Deliver every member through the outbox — collect_map requires all to
    // complete.
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

    let settled = wait_for_history(&store, &workflow_id, "map settled", |events| {
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
    assert_eq!(
        count_cancelled(&settled),
        0,
        "a fully-completing collect_map cancels nothing"
    );
    assert_eq!(count_failed(&settled), 0, "no member failed");

    // collect_map returns the per-ordinal results in input order.
    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("collect_map_four failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(
        value,
        json!([
            worker_result(0),
            worker_result(1),
            worker_result(2),
            worker_result(3),
        ]),
        "collect_map must return all {FAN_OUT} results in input order"
    );

    let completed_terminals = settled
        .iter()
        .filter(|event| matches!(event, Event::WorkflowCompleted { .. }))
        .count();
    assert_eq!(
        completed_terminals, 1,
        "the map workflow completes exactly once"
    );
    assert!(
        !fired.load(Ordering::SeqCst),
        "in-process dispatcher must never fire under the outbox flag"
    );

    engine.shutdown()?;
    Ok(())
}
