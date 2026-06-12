//! Flagship order-fulfillment saga end-to-end tests.
//!
//! These tests drive the packaged `examples/order-fulfillment` Gleam
//! workflows through the real engine, in one coherent business flow per
//! test: payment charge with a recorded transient failure and a
//! workflow-driven retry over a durable backoff sleep, a human approval
//! signal raced against a durable deadline, an `order_shipping` child
//! workflow, an `order_status` query answered at every parked stage, refund
//! compensation on rejection/timeout, a mid-flight engine kill+restart
//! durability proof, and a mid-flight v2 deploy with version pinning.
//!
//! The archives are build artifacts, so every test skips (with a notice)
//! when they have not been produced yet:
//! `cargo run -p aion-cli -- package examples/order-fulfillment --build`.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use aion::activity::bridge::ActivityDispatcher;
use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, EngineError, QueryError, RuntimeHandle, SignalRouter};
use aion_core::{Event, Payload, RunId, WorkflowId, WorkflowStatus, status_from_events};
use aion_package::{BeamModule, BeamSet, ExtractionLimits, Package, PackageBuilder};
use aion_store::{EventStore, InMemoryStore};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PARENT_TYPE: &str = "order_fulfillment";
/// KNOWN DEFECT BUDGET (beamr VM, observed on 0.4.6/0.4.9/0.5.0): any
/// activity result or failure payload the engine delivers to a
/// **Gleam-compiled** workflow at an await dies with
/// `VM execution error: bad argument` once it exceeds 64 bytes (63- and
/// 64-byte payloads work; 65 bytes kills the process; Erlang-coded
/// workflows are unaffected, as is the workflow *start* input path).
/// Every payload this test's dispatcher returns is kept inside that budget
/// and checked loudly here, so a future edit fails with a clear message
/// instead of an opaque VM crash. Remove this cap when the beamr fix lands.
const ENGINE_TO_GLEAM_PAYLOAD_BUDGET: usize = 64;
const CHILD_TYPE: &str = "order_shipping";
const STATUS_QUERY: &str = "order_status";
const APPROVAL_SIGNAL: &str = "approval_decision";

/// Generous engine reply deadline; queries in these tests must succeed.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for polling loops (query registration, stage transitions,
/// history predicates). Generous because registration is workflow code that
/// races the caller after `start_workflow`/recovery returns.
const POLL_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

fn examples_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/order-fulfillment")
}

/// Load both saga archives, or `None` (with a skip notice) when not built.
fn saga_packages(test: &str) -> Result<Option<(Package, Package)>, Box<dyn std::error::Error>> {
    let root = examples_root();
    let parent_path = root.join("order-fulfillment.aion");
    let child_path = root.join("order-shipping.aion");
    if !parent_path.exists() || !child_path.exists() {
        eprintln!(
            "skipping {test}: {} / {} not built (run `cargo run -p aion-cli -- package \
             examples/order-fulfillment --build`)",
            parent_path.display(),
            child_path.display()
        );
        return Ok(None);
    }
    let parent =
        Package::load_from_bytes(std::fs::read(&parent_path)?, ExtractionLimits::unbounded())?;
    let child =
        Package::load_from_bytes(std::fs::read(&child_path)?, ExtractionLimits::unbounded())?;
    Ok(Some((parent, child)))
}

/// A one-shot release gate the test holds while it queries a parked stage.
///
/// Activity dispatch runs on the blocking pool, so an activity blocked here
/// parks its workflow at the corresponding await yield point — where the
/// query pump services `order_status` — without wedging the engine.
struct Gate {
    released: Mutex<bool>,
    condvar: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            released: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    fn release(&self) {
        match self.released.lock() {
            Ok(mut released) => {
                *released = true;
                self.condvar.notify_all();
            }
            Err(poisoned) => {
                let mut released = poisoned.into_inner();
                *released = true;
                self.condvar.notify_all();
            }
        }
    }

    fn wait(&self) -> Result<(), String> {
        let released = self
            .released
            .lock()
            .map_err(|_| "gate mutex poisoned".to_owned())?;
        let (_released, timeout) = self
            .condvar
            .wait_timeout_while(released, POLL_DEADLINE, |released| !*released)
            .map_err(|_| "gate mutex poisoned".to_owned())?;
        if timeout.timed_out() {
            return Err("gate wait timed out".to_owned());
        }
        Ok(())
    }
}

/// One recorded activity dispatch as the engine seam delivered it.
#[derive(Clone, Debug)]
struct Call {
    name: String,
    input: Value,
    config: Value,
    /// One-based delivery attempt stamped by the engine on the wire.
    attempt: u32,
}

/// Deterministic in-process stand-in for the remote activity worker.
///
/// `charge_payment` fails its first business attempt (the attempt number the
/// workflow encodes into the activity input) with a retryable error and
/// succeeds afterwards; optional gates let tests hold an activity open while
/// they query the parked workflow.
struct SagaDispatcher {
    calls: Mutex<Vec<Call>>,
    charge_gate: Option<Arc<Gate>>,
    ship_gate: Option<Arc<Gate>>,
}

impl SagaDispatcher {
    fn new(charge_gate: Option<Arc<Gate>>, ship_gate: Option<Arc<Gate>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            charge_gate,
            ship_gate,
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    fn calls_named(&self, name: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.name == name)
            .collect()
    }
}

/// Refuse to return a payload that would trip the beamr >64-byte
/// engine-to-Gleam delivery defect (see [`ENGINE_TO_GLEAM_PAYLOAD_BUDGET`]).
fn budgeted(payload: String) -> Result<String, String> {
    if payload.len() > ENGINE_TO_GLEAM_PAYLOAD_BUDGET {
        return Err(format!(
            "terminal:test payload {} bytes exceeds the {ENGINE_TO_GLEAM_PAYLOAD_BUDGET}-byte \
             beamr engine-to-Gleam delivery budget: {payload}",
            payload.len()
        ));
    }
    Ok(payload)
}

impl ActivityDispatcher for SagaDispatcher {
    fn dispatch(
        &self,
        name: &str,
        input: &str,
        config: &str,
        attempt: u32,
    ) -> Result<String, String> {
        let input_value: Value =
            serde_json::from_str(input).map_err(|e| format!("terminal:bad input: {e}"))?;
        let config_value: Value =
            serde_json::from_str(config).map_err(|e| format!("terminal:bad config: {e}"))?;
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(Call {
                name: name.to_owned(),
                input: input_value.clone(),
                config: config_value,
                attempt,
            });
        }
        let order_id = input_value["order_id"]
            .as_str()
            .ok_or_else(|| format!("terminal:{name} input missing order_id"))?
            .to_owned();
        match name {
            "charge_payment" => {
                let business_attempt = input_value["attempt"]
                    .as_i64()
                    .ok_or_else(|| "terminal:charge input missing attempt".to_owned())?;
                if business_attempt == 1 {
                    // The full message (prefix included) must stay inside the
                    // 64-byte engine-to-Gleam delivery budget.
                    return Err("retryable:payment gateway unavailable (transient)".to_owned());
                }
                if let Some(gate) = &self.charge_gate {
                    gate.wait().map_err(|reason| format!("terminal:{reason}"))?;
                }
                budgeted(
                    json!({
                        "order_id": order_id,
                        "payment_id": format!("pay-{order_id}"),
                        "amount_cents": input_value["amount_cents"],
                    })
                    .to_string(),
                )
            }
            "ship_order" => {
                if let Some(gate) = &self.ship_gate {
                    gate.wait().map_err(|reason| format!("terminal:{reason}"))?;
                }
                budgeted(
                    json!({
                        "order_id": order_id,
                        "shipment_id": format!("ship-{order_id}"),
                        "carrier": "ae",
                    })
                    .to_string(),
                )
            }
            "refund_payment" => budgeted(
                json!({
                    "order_id": order_id,
                    "refund_id": format!("re-{order_id}"),
                })
                .to_string(),
            ),
            other => Err(format!("terminal:unknown activity {other}")),
        }
    }
}

async fn engine_over(
    store: &Arc<dyn EventStore>,
    dispatcher: &Arc<SagaDispatcher>,
    packages: Vec<Package>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    let mut builder = EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .query_timeout(QUERY_TIMEOUT)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::clone(dispatcher) as Arc<dyn ActivityDispatcher>);
    for package in packages {
        builder = builder.load_workflows(package);
    }
    Ok(builder.build().await?)
}

fn order_input(
    order_id: &str,
    approval_timeout_ms: u64,
) -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({
        "order_id": order_id,
        "item": "widget",
        "quantity": 2,
        "amount_cents": 4999,
        "approval_timeout_ms": approval_timeout_ms,
    }))
}

fn approval(decision: &str, approver: &str) -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "decision": decision, "approver": approver }))
}

async fn start_order(
    engine: &Engine,
    order_id: &str,
    approval_timeout_ms: u64,
) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            PARENT_TYPE,
            order_input(order_id, approval_timeout_ms)?,
            HashMap::new(),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

/// Poll `order_status` until a reply satisfies `accept`, returning it.
///
/// `UnknownQuery` means the workflow has not reached its registration code
/// yet (registration is workflow code racing the caller); a reply that is
/// not accepted means the saga has not advanced yet. Both retry until the
/// deadline.
async fn status_matching<F>(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    want: &str,
    accept: F,
) -> Result<Value, Box<dyn std::error::Error>>
where
    F: Fn(&Value) -> bool,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut last: Option<Value> = None;
    loop {
        match engine.query(workflow_id, run_id, STATUS_QUERY).await {
            Ok(payload) => {
                let value: Value = serde_json::from_slice(payload.bytes())?;
                if accept(&value) {
                    return Ok(value);
                }
                last = Some(value);
            }
            Err(EngineError::Query(QueryError::UnknownQuery(_))) => {}
            Err(other) => {
                let history = engine.store().read_history(workflow_id).await?;
                return Err(format!(
                    "query failed before `{want}` was observed: {other}; \
                     last reply: {last:?}; history: {history:#?}"
                )
                .into());
            }
        }
        if Instant::now() >= deadline {
            return Err(
                format!("`{want}` not reached before deadline; last reply: {last:?}").into(),
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `order_status` until the saga reports stage `want`.
async fn status_at_stage(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    want: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    status_matching(engine, workflow_id, run_id, want, |value| {
        value["stage"] == json!(want)
    })
    .await
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
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let history = store.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {description}: {history:#?}").into());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn completed_result(
    engine: &Engine,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<Value, Box<dyn std::error::Error>> {
    let result = engine.result(workflow_id, run_id).await?;
    let history = engine.store().read_history(workflow_id).await?;
    let payload =
        result.map_err(|error| format!("saga failed: {error:?}; history: {history:#?}"))?;
    Ok(serde_json::from_slice(payload.bytes())?)
}

fn count<F>(history: &[Event], predicate: F) -> usize
where
    F: Fn(&Event) -> bool,
{
    history.iter().filter(|event| predicate(event)).count()
}

fn charge_scheduled(event: &Event) -> bool {
    matches!(
        event,
        Event::ActivityScheduled { activity_type, .. } if activity_type == "charge_payment"
    )
}

/// Drive the saga from start to `awaiting_approval` on the first engine:
/// observe the gated retry attempt through the query, then assert the
/// transient failure, the backoff timer, and the dispatch records are
/// durable facts. Returns the history as recorded just before the kill.
async fn charge_retries_then_awaits_approval(
    engine: &Engine,
    dispatcher: &SagaDispatcher,
    charge_gate: &Gate,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    // Capability 4 (query) + capability 1 (retry): while the gated retry
    // attempt is in flight, the saga reports `charging` on attempt 2. (The
    // first, transiently-failing attempt also answers `charging`, so the
    // poll accepts only the retry attempt.)
    let charging = status_matching(engine, workflow_id, run_id, "charging attempt 2", |value| {
        value["stage"] == json!("charging") && value["payment_attempts"] == json!(2)
    })
    .await?;
    assert_eq!(charging["order_id"], json!("o1"));
    assert_eq!(charging["payment_id"], Value::Null);
    charge_gate.release();

    let awaiting = status_at_stage(engine, workflow_id, run_id, "awaiting_approval").await?;
    assert_eq!(awaiting["payment_attempts"], json!(2));
    assert_eq!(awaiting["payment_id"], json!("pay-o1"));

    // The transient failure and the backoff timer are durable facts.
    let pre_kill = wait_for_history(store, workflow_id, "approval deadline armed", |history| {
        count(history, |event| matches!(event, Event::TimerStarted { .. })) >= 2
    })
    .await?;
    assert_eq!(
        count(&pre_kill, charge_scheduled),
        2,
        "history: {pre_kill:#?}"
    );
    // ROUGH EDGE: the in-VM dispatcher seam classifies every dispatcher
    // failure `ActivityErrorKind::Terminal` and leaves the `retryable:`
    // prefix in the recorded message (`crates/aion/src/runtime/handle/
    // delivery.rs` `activity_failure`), even though the Gleam SDK parses
    // that prefix into a typed `Retryable`. Until the seam honours the
    // prefix, the durable record is identified by message, not kind.
    assert_eq!(
        count(&pre_kill, |event| matches!(
            event,
            Event::ActivityFailed { error, attempt, .. }
                if error.message.starts_with("retryable:") && *attempt == 1
        )),
        1,
        "exactly one recorded transient charge failure: {pre_kill:#?}"
    );
    let first_calls = dispatcher.calls_named("charge_payment");
    assert_eq!(
        first_calls
            .iter()
            .map(|call| call.input["attempt"].clone())
            .collect::<Vec<_>>(),
        vec![json!(1), json!(2)],
        "the retry loop re-dispatches with the next business attempt"
    );
    for call in &first_calls {
        // The declared RetryPolicy rides the dispatch config verbatim, but
        // the engine does not yet consume it: every wire delivery is
        // stamped attempt 1 (engine-side automatic retry is unbuilt), which
        // is exactly why the workflow drives its own bounded retry loop.
        assert_eq!(call.config["retry"]["max_attempts"], json!(3));
        assert_eq!(call.config["retry"]["backoff"]["kind"], json!("fixed"));
        assert_eq!(call.config["retry"]["backoff"]["delay_ms"], json!(50));
        assert_eq!(call.attempt, 1, "wire attempt is always 1 today");
    }
    Ok(pre_kill)
}

/// Approve the recovered run, observe the parked `shipping` stage while the
/// child's activity is gated, then release it and assert the completed
/// business result. Returns the child workflow id recorded by the parent.
async fn approval_ships_child_to_completion(
    engine: &Engine,
    dispatcher: &SagaDispatcher,
    ship_gate: &Gate,
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<WorkflowId, Box<dyn std::error::Error>> {
    // Capability 2 (signal wins the race) on the post-restart engine.
    engine
        .signal(
            workflow_id,
            run_id,
            APPROVAL_SIGNAL,
            approval("approve", "cfo")?,
        )
        .await?;

    // Capability 3 (child workflow) + capability 4: the gated `ship_order`
    // activity holds the child open, so the parent reports `shipping`.
    let shipping = status_at_stage(engine, workflow_id, run_id, "shipping").await?;
    assert_eq!(shipping["payment_id"], json!("pay-o1"));
    let with_child = store.read_history(workflow_id).await?;
    let child_workflow_id = with_child
        .iter()
        .find_map(|event| match event {
            Event::ChildWorkflowStarted {
                child_workflow_id,
                workflow_type,
                ..
            } if workflow_type == CHILD_TYPE => Some(child_workflow_id.clone()),
            _ => None,
        })
        .ok_or("parent recorded no ChildWorkflowStarted for order_shipping")?;
    ship_gate.release();

    let output = completed_result(engine, workflow_id, run_id).await?;
    assert_eq!(output["status"], json!("completed"), "output: {output}");
    assert_eq!(output["payment_id"], json!("pay-o1"));
    assert_eq!(output["shipment_id"], json!("ship-o1"));
    assert_eq!(output["refund_id"], Value::Null);
    assert_eq!(output["reason"], json!("approved by cfo"));

    // Post-restart the dispatcher saw only the child's activity: the charge
    // attempts were resolved from recorded history, never re-executed.
    let calls = dispatcher.calls();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>(),
        vec!["ship_order"],
        "replay must not re-dispatch recorded activities: {calls:?}"
    );
    Ok(child_workflow_id)
}

/// Assert the completed parent and child histories tell the whole saga
/// story: two charge dispatches, one approval signal, the backoff timer
/// fired, the approval deadline cancelled, and the child completed with its
/// own recorded activity.
fn assert_completed_saga_history(
    final_history: &[Event],
    child_history: &[Event],
    child_workflow_id: &WorkflowId,
) {
    assert_eq!(status_from_events(final_history), WorkflowStatus::Completed);
    assert_eq!(count(final_history, charge_scheduled), 2);
    assert_eq!(
        count(final_history, |event| matches!(
            event,
            Event::SignalReceived { name, .. } if name == APPROVAL_SIGNAL
        )),
        1
    );
    assert!(
        count(final_history, |event| matches!(
            event,
            Event::TimerFired { .. }
        )) >= 1,
        "the retry backoff sleep must record TimerFired: {final_history:#?}"
    );
    assert!(
        count(final_history, |event| matches!(
            event,
            Event::TimerCancelled { .. }
        )) >= 1,
        "the signal-won approval deadline must record TimerCancelled: {final_history:#?}"
    );
    assert_eq!(
        count(final_history, |event| matches!(
            event,
            Event::ChildWorkflowCompleted { child_workflow_id: id, .. } if id == child_workflow_id
        )),
        1
    );

    // The child ran its own recorded activity in its own history.
    assert_eq!(status_from_events(child_history), WorkflowStatus::Completed);
    assert_eq!(
        count(child_history, |event| matches!(
            event,
            Event::ActivityScheduled { activity_type, .. } if activity_type == "ship_order"
        )),
        1,
        "child history: {child_history:#?}"
    );
}

/// The full happy path with a mid-flight engine kill: charge fails once and
/// is retried over a durable backoff sleep, the engine is killed and
/// restarted while the saga waits for approval, replay restores the exact
/// recorded history (and the query handler), and the post-restart approval
/// signal drives the shipping child workflow to completion.
#[tokio::test]
async fn order_completes_after_payment_retry_engine_restart_and_approval() -> TestResult {
    let Some((parent, child)) =
        saga_packages("order_completes_after_payment_retry_engine_restart_and_approval")?
    else {
        return Ok(());
    };
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());

    // Engine #1: hold the second charge attempt open so the parked
    // `charging` stage is observable through the query.
    let charge_gate = Arc::new(Gate::new());
    let first_dispatcher = Arc::new(SagaDispatcher::new(Some(Arc::clone(&charge_gate)), None));
    let first_engine = engine_over(
        &store,
        &first_dispatcher,
        vec![parent.clone(), child.clone()],
    )
    .await?;
    let (workflow_id, run_id) = start_order(&first_engine, "o1", 600_000).await?;
    let pre_kill = charge_retries_then_awaits_approval(
        &first_engine,
        &first_dispatcher,
        &charge_gate,
        &store,
        &workflow_id,
        &run_id,
    )
    .await?;

    // Capability 6 (durability): kill the engine mid-flight — after payment,
    // before approval — and restart over the same store.
    first_engine.shutdown()?;
    let ship_gate = Arc::new(Gate::new());
    let second_dispatcher = Arc::new(SagaDispatcher::new(None, Some(Arc::clone(&ship_gate))));
    let second_engine = engine_over(
        &store,
        &second_dispatcher,
        vec![parent.clone(), child.clone()],
    )
    .await?;

    // Replay restored the exact recorded history: no duplicated events, no
    // re-executed activities.
    let post_recovery = store.read_history(&workflow_id).await?;
    assert_eq!(
        post_recovery, pre_kill,
        "replay must not change durable history"
    );
    assert_eq!(
        count(&post_recovery, |event| matches!(
            event,
            Event::WorkflowStarted { .. }
        )),
        1
    );

    // The recovered run answers the same query without extra author code.
    let recovered =
        status_at_stage(&second_engine, &workflow_id, &run_id, "awaiting_approval").await?;
    assert_eq!(recovered["payment_id"], json!("pay-o1"));

    let child_workflow_id = approval_ships_child_to_completion(
        &second_engine,
        &second_dispatcher,
        &ship_gate,
        &store,
        &workflow_id,
        &run_id,
    )
    .await?;
    assert!(first_dispatcher.calls_named("refund_payment").is_empty());

    // Durable history tells the whole story.
    let final_history = store.read_history(&workflow_id).await?;
    let child_history = store.read_history(&child_workflow_id).await?;
    assert_completed_saga_history(&final_history, &child_history, &child_workflow_id);

    // A terminal workflow answers queries with the typed NotRunning error.
    match second_engine
        .query(&workflow_id, &run_id, STATUS_QUERY)
        .await
    {
        Err(EngineError::Query(QueryError::NotRunning(_))) => {}
        other => return Err(format!("expected NotRunning after terminal, got {other:?}").into()),
    }

    second_engine.shutdown()?;
    Ok(())
}

/// Capability 5 (compensation): a rejection signal refunds the captured
/// payment and completes the order as business-`cancelled`.
#[tokio::test]
async fn order_cancels_and_refunds_when_rejected() -> TestResult {
    let Some((parent, child)) = saga_packages("order_cancels_and_refunds_when_rejected")? else {
        return Ok(());
    };
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let dispatcher = Arc::new(SagaDispatcher::new(None, None));
    let engine = engine_over(&store, &dispatcher, vec![parent, child]).await?;

    let (workflow_id, run_id) = start_order(&engine, "o2", 600_000).await?;
    status_at_stage(&engine, &workflow_id, &run_id, "awaiting_approval").await?;
    engine
        .signal(
            &workflow_id,
            &run_id,
            APPROVAL_SIGNAL,
            approval("reject", "auditor")?,
        )
        .await?;

    let output = completed_result(&engine, &workflow_id, &run_id).await?;
    assert_eq!(output["status"], json!("cancelled"), "output: {output}");
    assert_eq!(output["payment_id"], json!("pay-o2"));
    assert_eq!(output["refund_id"], json!("re-o2"));
    assert_eq!(output["shipment_id"], Value::Null);
    assert_eq!(output["reason"], json!("rejected by auditor"));

    let names: Vec<String> = dispatcher
        .calls()
        .into_iter()
        .map(|call| call.name)
        .collect();
    assert_eq!(
        names,
        vec!["charge_payment", "charge_payment", "refund_payment"],
        "rejection must refund and never ship"
    );

    let history = store.read_history(&workflow_id).await?;
    // A compensated saga is a successful run: the workflow projection is
    // Completed while the business outcome is cancelled.
    assert_eq!(status_from_events(&history), WorkflowStatus::Completed);
    assert_eq!(
        count(&history, |event| matches!(
            event,
            Event::ChildWorkflowStarted { .. }
        )),
        0
    );
    assert!(
        count(&history, |event| matches!(
            event,
            Event::TimerCancelled { .. }
        )) >= 1,
        "the rejected signal still cancels the approval deadline: {history:#?}"
    );

    engine.shutdown()?;
    Ok(())
}

/// Capability 2 + 5 (timeout side of the race): no decision arrives, the
/// durable deadline fires, and the saga refunds and cancels.
#[tokio::test]
async fn order_cancels_and_refunds_when_approval_times_out() -> TestResult {
    let Some((parent, child)) = saga_packages("order_cancels_and_refunds_when_approval_times_out")?
    else {
        return Ok(());
    };
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let dispatcher = Arc::new(SagaDispatcher::new(None, None));
    let engine = engine_over(&store, &dispatcher, vec![parent, child]).await?;

    // The 1-second approval deadline is the business timer under test; the
    // test only awaits the workflow result.
    let (workflow_id, run_id) = start_order(&engine, "o3", 1_000).await?;
    let output = completed_result(&engine, &workflow_id, &run_id).await?;
    assert_eq!(output["status"], json!("cancelled"), "output: {output}");
    assert_eq!(output["refund_id"], json!("re-o3"));
    assert_eq!(output["shipment_id"], Value::Null);
    assert_eq!(output["reason"], json!("approval timed out after 1000ms"));

    let names: Vec<String> = dispatcher
        .calls()
        .into_iter()
        .map(|call| call.name)
        .collect();
    assert_eq!(
        names,
        vec!["charge_payment", "charge_payment", "refund_payment"],
        "timeout must refund and never ship"
    );

    let history = store.read_history(&workflow_id).await?;
    assert_eq!(status_from_events(&history), WorkflowStatus::Completed);
    assert_eq!(
        count(&history, |event| matches!(
            event,
            Event::SignalReceived { .. }
        )),
        0
    );
    assert!(
        count(&history, |event| matches!(event, Event::TimerFired { .. })) >= 2,
        "both the backoff sleep and the approval deadline fire: {history:#?}"
    );

    engine.shutdown()?;
    Ok(())
}

const V2_MARKER_MODULE: &str = "order_fulfillment_v2_marker";

/// Compile a trivial marker module so v2 has a distinct beam set (and
/// therefore a distinct content hash) while keeping v1's behavior.
fn compile_marker_beam() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let temp_dir = std::env::temp_dir().join(format!("aion-order-saga-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp_dir)?;
    let source_path = temp_dir.join(format!("{V2_MARKER_MODULE}.erl"));
    std::fs::write(
        &source_path,
        format!("-module({V2_MARKER_MODULE}).\n-export([deployed/0]).\ndeployed() -> v2.\n"),
    )?;
    let status = Command::new("erlc")
        .arg("-o")
        .arg(&temp_dir)
        .arg(&source_path)
        .status()?;
    if !status.success() {
        let cleanup = std::fs::remove_dir_all(&temp_dir);
        drop(cleanup);
        return Err(format!("erlc failed with status {status}").into());
    }
    let bytes = std::fs::read(temp_dir.join(format!("{V2_MARKER_MODULE}.beam")))?;
    std::fs::remove_dir_all(temp_dir)?;
    Ok(bytes)
}

/// Rebuild the parent archive with one extra marker module: same entry,
/// same behavior, new content hash — a realistic "redeploy" of the saga.
fn second_version_of(parent: &Package) -> Result<Package, Box<dyn std::error::Error>> {
    let mut modules: Vec<BeamModule> = parent
        .beams()
        .iter()
        .map(|module| BeamModule::new(module.name(), module.bytes().to_vec()))
        .collect();
    modules.push(BeamModule::new(V2_MARKER_MODULE, compile_marker_beam()?));
    let beams = BeamSet::new(modules)?;
    let archive = PackageBuilder::new(parent.manifest().clone(), beams).write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

fn recorded_version(
    history: &[Event],
    run_id: &RunId,
) -> Result<aion_core::PackageVersion, Box<dyn std::error::Error>> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted {
                run_id: started_run,
                package_version,
                ..
            } if started_run == run_id => Some(package_version.clone()),
            _ => None,
        })
        .ok_or_else(|| "run has no WorkflowStarted".into())
}

/// Capability 7 (versioned deploy): a v2 of the saga is deployed while a v1
/// run waits for approval mid-flight. The pinned v1 run blocks unload,
/// completes on v1 after its signal, and new starts land on v2.
#[tokio::test]
async fn v2_deploy_mid_flight_pins_v1_and_routes_new_orders_to_v2() -> TestResult {
    let Some((v1, child)) =
        saga_packages("v2_deploy_mid_flight_pins_v1_and_routes_new_orders_to_v2")?
    else {
        return Ok(());
    };
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let dispatcher = Arc::new(SagaDispatcher::new(None, None));
    let engine = engine_over(&store, &dispatcher, vec![v1.clone(), child]).await?;

    // Run A reaches awaiting-approval on v1 and stays pinned there.
    let (pinned_id, pinned_run) = start_order(&engine, "o4", 600_000).await?;
    status_at_stage(&engine, &pinned_id, &pinned_run, "awaiting_approval").await?;

    // Deploy v2 into the RUNNING engine.
    let v2 = second_version_of(&v1)?;
    assert_ne!(v1.content_hash(), v2.content_hash());
    let loaded = engine.load_package(v2.clone()).await?;
    assert!(loaded.freshly_loaded, "first v2 load must be fresh");
    assert!(loaded.route_changed, "v2 must take the route");

    // The mid-flight v1 run pins its version: unload is refused with the
    // typed VersionPinned error.
    match engine
        .unload_workflow_version(PARENT_TYPE, v1.content_hash())
        .await
    {
        Err(EngineError::VersionPinned { workflow_type, .. }) => {
            assert_eq!(workflow_type, PARENT_TYPE);
        }
        other => return Err(format!("expected VersionPinned, got {other:?}").into()),
    }

    // A new order starts on v2 and runs the whole saga there.
    let (new_id, new_run) = start_order(&engine, "o5", 600_000).await?;
    status_at_stage(&engine, &new_id, &new_run, "awaiting_approval").await?;
    engine
        .signal(
            &new_id,
            &new_run,
            APPROVAL_SIGNAL,
            approval("approve", "ops")?,
        )
        .await?;
    let new_output = completed_result(&engine, &new_id, &new_run).await?;
    assert_eq!(new_output["status"], json!("completed"));
    assert_eq!(new_output["shipment_id"], json!("ship-o5"));
    let new_history = store.read_history(&new_id).await?;
    assert_eq!(
        recorded_version(&new_history, &new_run)?,
        aion_core::PackageVersion::new(v2.content_hash().to_string()),
        "the new run must record v2"
    );

    // The pinned run still completes on v1 after its approval arrives.
    engine
        .signal(
            &pinned_id,
            &pinned_run,
            APPROVAL_SIGNAL,
            approval("approve", "cfo")?,
        )
        .await?;
    let pinned_output = completed_result(&engine, &pinned_id, &pinned_run).await?;
    assert_eq!(pinned_output["status"], json!("completed"));
    assert_eq!(pinned_output["shipment_id"], json!("ship-o4"));
    let pinned_history = store.read_history(&pinned_id).await?;
    assert_eq!(
        recorded_version(&pinned_history, &pinned_run)?,
        aion_core::PackageVersion::new(v1.content_hash().to_string()),
        "the pinned run must record v1"
    );

    // With the v1 run terminal, the pin is gone and v1 unloads cleanly.
    // (The registry releases the handle asynchronously after the terminal
    // event, so poll the unload until the pin clears.)
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        match engine
            .unload_workflow_version(PARENT_TYPE, v1.content_hash())
            .await
        {
            Ok(()) => break,
            Err(EngineError::VersionPinned { .. }) if Instant::now() < deadline => {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Err(other) => return Err(format!("unload after terminal failed: {other}").into()),
        }
    }
    let remaining: Vec<_> = engine
        .list_workflow_versions()?
        .into_iter()
        .filter(|info| info.workflow_type == PARENT_TYPE)
        .collect();
    assert_eq!(remaining.len(), 1, "versions: {remaining:?}");
    assert_eq!(&remaining[0].content_hash, v2.content_hash());
    assert!(remaining[0].route_active);

    engine.shutdown()?;
    Ok(())
}
