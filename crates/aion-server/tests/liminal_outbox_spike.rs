//! #13-0/#13-1: outbox fan-out dispatch over liminal, result back through the
//! existing `OutboxDeliveryCallback`, behind the `liminal-transport` feature.
//!
//! The whole file is gated on the feature, so a default build never compiles it
//! and never links liminal. It stands up a REAL `liminal-server` over loopback
//! TCP, drives `LiminalOutboxDispatch::dispatch` (the dispatch-out seam) across
//! the socket, and routes a worker result back through `LiminalCompletionSource`
//! into a recording `OutboxDeliveryCallback` (the completion-return seam) — the
//! same callback trait the gRPC completion path uses.
//!
//! 13-1 adds the honest-ack retry composition: `LiminalOutboxDispatch` now keys
//! liminal dedup-on-delivery on a PER-ATTEMPT key (`{dispatch_key}#{attempt}`),
//! so a legitimate outbox retry is a fresh, non-suppressed publish. The
//! `legit_retry_*` tests prove a bumped-attempt re-dispatch succeeds (no longer
//! self-suppressed), a same-attempt duplicate is still deduped, and the full
//! `OutboxDispatcher` retry loop drives a no-worker dispatch through backoff to a
//! successful re-dispatch once a worker subscribes.
//!
//! Honest scope: liminal's request-reply responder is an in-server echo
//! participant (13-L0), so the "worker" that returns the result is liminal's
//! echo, not a separate aion worker binary. The wire is genuine (real TCP, real
//! correlation, real dedup-on-delivery, real delivery ack); registering an
//! external aion worker as the responder is deferred to 13-3/13-6. This test
//! proves the aion-side wiring and the dedup<->retry composition over the real
//! wire.
#![cfg(feature = "liminal-transport")]

use std::error::Error;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use aion_core::{ActivityId, ContentType, Payload, RunId, WorkflowId};
use aion_server::worker::liminal_transport::{
    DispatchRequest, DispatchResponse, LiminalCompletionSource, LiminalOutboxDispatch,
    attempt_idempotency_key,
};
use aion_server::worker::{
    OutboxDeliveryCallback, OutboxDispatcher, OutboxDispatcherConfig, OutboxRowDispatch,
};
use aion_store::{OutboxRow, OutboxStatus, OutboxStore};
use aion_store_libsql::LibSqlStore;
use chrono::Utc;
use futures::Stream;
use liminal_sdk::{
    ChannelHandle, ConnectionPoolConfig, ConversationHandle, RemoteChannelHandle, RemoteConfig,
    RemoteConversationHandle,
};
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::ConnectionSupervisor;
use liminal_server::server::listener::ServerListener;
use tokio::sync::watch;
use uuid::Uuid;

type TestError = Box<dyn Error + Send + Sync>;

/// One recorded completion delivery: the correlation ids plus the result.
type CompletionRecord = (WorkflowId, ActivityId, Option<RunId>, String);

const CHANNEL: &str = "aion.activities";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Wraps any displayable error as a `Send + Sync` test error.
fn test_error(message: impl std::fmt::Display) -> TestError {
    message.to_string().into()
}

/// Recording [`OutboxDeliveryCallback`] standing in for the prod
/// `ServerOutboxDeliveryCallback`. Records each delivery so the test can assert
/// the worker result genuinely re-entered aion through the same seam.
#[derive(Debug, Default)]
struct RecordingCallback {
    completions: Mutex<Vec<CompletionRecord>>,
}

impl OutboxDeliveryCallback for RecordingCallback {
    fn deliver_completion(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
        run_id: Option<&RunId>,
        result: String,
    ) -> Result<bool, aion_server::ServerError> {
        if let Ok(mut completions) = self.completions.lock() {
            completions.push((
                workflow_id.clone(),
                activity_id.clone(),
                run_id.cloned(),
                result,
            ));
        }
        Ok(true)
    }

    fn deliver_failure(
        &self,
        _workflow_id: &WorkflowId,
        _activity_id: &ActivityId,
        _run_id: Option<&RunId>,
        _reason: String,
    ) -> Result<bool, aion_server::ServerError> {
        Ok(true)
    }
}

/// Drives a synchronous `ReadyResult`-style future to completion.
fn block_on<F: Future>(future: F) -> Result<F::Output, TestError> {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => Ok(value),
        Poll::Pending => Err(test_error(
            "synchronous transport future parked unexpectedly",
        )),
    }
}

/// Holds the running liminal server bound for the lifetime of a test.
struct RunningServer {
    listener: Option<ServerListener>,
    supervisor: ConnectionSupervisor,
    address: SocketAddr,
}

impl RunningServer {
    fn start() -> Result<Self, TestError> {
        let config = ServerConfig {
            listen_address: "127.0.0.1:0".parse().map_err(test_error)?,
            health_listen_address: reserve_loopback_port()?,
            channels: vec![ChannelDef {
                name: CHANNEL.to_owned(),
                schema_ref: "schemas/activities.json".to_owned(),
                durable: false,
            }],
            routing_rules: Vec::new(),
            persistence_path: None,
            cluster: None,
            drain_timeout_ms: 30_000,
        };
        let supervisor = ConnectionSupervisor::from_config(&config).map_err(test_error)?;
        let listener = ServerListener::bind(&config, supervisor).map_err(test_error)?;
        let supervisor = listener.supervisor();
        let address = listener.local_addr();
        Ok(Self {
            listener: Some(listener),
            supervisor,
            address,
        })
    }

    const fn address(&self) -> SocketAddr {
        self.address
    }

    fn wait_for_connection(&self) -> Result<(), TestError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if self.supervisor.active_connection_count() >= 1 {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(test_error(
            "liminal server never observed a live client connection",
        ))
    }

    fn shutdown(mut self) -> Result<(), TestError> {
        if let Some(listener) = self.listener.take() {
            listener.shutdown().map_err(test_error)?;
        }
        Ok(())
    }
}

fn reserve_loopback_port() -> Result<SocketAddr, TestError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(test_error)?;
    let address = listener.local_addr().map_err(test_error)?;
    drop(listener);
    Ok(address)
}

/// Subscribes a "worker" on the activity channel so a delivery is observable,
/// driving the `Subscribe` -> `SubscribeAck` round trip over the socket.
fn subscribe_worker(handle: &RemoteChannelHandle) -> Result<(), TestError> {
    let subscription = handle.subscribe::<DispatchRequest>();
    let mut subscription = pin!(subscription);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    match subscription.as_mut().poll_next(&mut context) {
        Poll::Ready(None) => Ok(()),
        Poll::Ready(Some(Err(error))) => Err(test_error(format!("subscribe setup error: {error}"))),
        Poll::Ready(Some(Ok(_))) => Err(test_error("subscribe unexpectedly yielded a message")),
        Poll::Pending => Err(test_error("subscribe stream parked unexpectedly")),
    }
}

/// Connects a remote channel handle to the running liminal server.
fn connect_channel(address: SocketAddr) -> Result<RemoteChannelHandle, TestError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        let config = RemoteConfig::new(
            address.to_string(),
            CHANNEL,
            CHANNEL,
            ConnectionPoolConfig::new(1, 10, 16),
        )
        .map_err(test_error)?;
        match config.connect_tcp() {
            Ok(connected) => return RemoteChannelHandle::new(&connected).map_err(test_error),
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    Err(last_error.map_or_else(
        || test_error("never connected"),
        |error| test_error(format!("never connected: {error}")),
    ))
}

/// Builds a remote conversation handle for the completion round trip.
fn build_conversation(address: SocketAddr) -> Result<RemoteConversationHandle, TestError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        let config = RemoteConfig::new(
            address.to_string(),
            CHANNEL,
            "completion",
            ConnectionPoolConfig::new(1, 10, 16),
        )
        .map_err(test_error)?;
        match config.connect_tcp() {
            Ok(connected) => return Ok(RemoteConversationHandle::new(&connected)),
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    Err(last_error.map_or_else(
        || test_error("never connected"),
        |error| test_error(format!("never connected: {error}")),
    ))
}

/// Builds a pending outbox row for a fresh workflow + ordinal.
fn pending_row(ordinal: u64) -> OutboxRow {
    let workflow_id = WorkflowId::new(Uuid::new_v4());
    let dispatch_key = format!("{workflow_id}:{ordinal}");
    OutboxRow {
        dispatch_key,
        workflow_id,
        ordinal,
        run_id: Some(RunId::new(Uuid::new_v4())),
        namespace: "default".to_owned(),
        task_queue: "default".to_owned(),
        activity_type: "charge-card".to_owned(),
        input: Payload::new(ContentType::Json, br#"{"amount":42}"#.to_vec()),
        status: OutboxStatus::Pending,
        attempt: 0,
        visible_after: Utc::now(),
        claimed_at: None,
    }
}

/// THE LOAD-BEARING TEST: a real outbox dispatch over liminal to a worker, and
/// the worker's result back through `OutboxDeliveryCallback`, happy path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_over_liminal_returns_result_through_callback() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address();

    // A worker subscribes so the keyed publish reports a genuine delivery.
    let worker = connect_channel(address)?;
    subscribe_worker(&worker)?;
    server.wait_for_connection()?;

    // DISPATCH-OUT SEAM: place one claimed row over liminal. dispatch_key is the
    // liminal idempotency key; a genuine delivery ack returns Ok(()).
    let dispatch = LiminalOutboxDispatch::new(address.to_string(), CHANNEL);
    let row = pending_row(0);
    dispatch
        .dispatch(&row)
        .await
        .map_err(|error| test_error(format!("dispatch returned Err: {error}")))?;

    // COMPLETION-RETURN SEAM: obtain the worker's result over the correlated
    // request-reply round trip (the echo participant returns the request), map
    // it to a DispatchResponse, and re-enter it into aion through the same
    // OutboxDeliveryCallback the gRPC path uses.
    let conversation = build_conversation(address)?;
    let request = DispatchRequest {
        activity_type: row.activity_type.clone(),
        workflow_id: row.workflow_id.clone(),
        ordinal: row.ordinal,
        run_id: row.run_id.clone(),
        input: row.input.bytes().to_vec(),
    };
    conversation.request(request).map_err(test_error)?;
    let echoed: DispatchRequest = block_on(conversation.receive())??;

    let callback = Arc::new(RecordingCallback::default());
    let source =
        LiminalCompletionSource::new(Arc::clone(&callback) as Arc<dyn OutboxDeliveryCallback>);
    let response = DispatchResponse {
        workflow_id: echoed.workflow_id.clone(),
        ordinal: echoed.ordinal,
        run_id: echoed.run_id.clone(),
        outcome: Ok(r#"{"charged":true}"#.to_owned()),
    };
    let delivered = source.deliver(&response).map_err(test_error)?;
    assert!(delivered, "completion must deliver to the live run");

    // The result re-entered aion through the callback, correlated to the exact
    // workflow/ordinal/run that was dispatched.
    let completions = callback
        .completions
        .lock()
        .map_err(|_| test_error("completions lock poisoned"))?;
    assert_eq!(completions.len(), 1, "exactly one completion delivered");
    let (workflow_id, activity_id, run_id, result) = completions
        .first()
        .ok_or_else(|| test_error("no completion"))?;
    assert_eq!(workflow_id, &row.workflow_id);
    assert_eq!(
        activity_id,
        &ActivityId::from_sequence_position(row.ordinal)
    );
    assert_eq!(
        run_id, &row.run_id,
        "run_id survived the liminal round trip"
    );
    assert_eq!(result, r#"{"charged":true}"#);
    drop(completions);

    server.shutdown()?;
    Ok(())
}

/// THE 13-1 FIX (renamed from 13-0's `redispatch_with_stable_key_is_suppressed_by_dedup`,
/// whose meaning is now INVERTED by the per-attempt-key fix): a legitimate outbox
/// retry — same row, `attempt` bumped, as `retry_outbox_row` produces — is a fresh
/// liminal publish that dedup-on-delivery does NOT suppress, so the re-dispatch
/// returns Ok with a live worker. This is the composition bug 13-0 documented as a
/// trap; 13-1 fixes it with `{dispatch_key}#{attempt}` keys.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legit_retry_with_bumped_attempt_is_not_suppressed() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address();

    let worker = connect_channel(address)?;
    subscribe_worker(&worker)?;
    server.wait_for_connection()?;

    let dispatch = LiminalOutboxDispatch::new(address.to_string(), CHANNEL);

    // First dispatch: attempt 0, fresh key + live worker => genuine delivery => Ok.
    let first = pending_row(0);
    dispatch
        .dispatch(&first)
        .await
        .map_err(|error| test_error(format!("first dispatch must succeed: {error}")))?;

    // The outbox retries by returning the SAME row to pending with `attempt`
    // bumped (and an unchanged stable dispatch_key). With the per-attempt key the
    // re-dispatch is a DISTINCT liminal idempotency key, so it is delivered, not
    // dedup-suppressed: the legitimate retry now succeeds.
    let mut retried = first.clone();
    retried.attempt += 1;
    assert_eq!(
        retried.dispatch_key, first.dispatch_key,
        "the outbox keeps the stable dispatch_key across retries"
    );
    dispatch
        .dispatch(&retried)
        .await
        .map_err(|error| test_error(format!("bumped-attempt retry must succeed: {error}")))?;

    server.shutdown()?;
    Ok(())
}

/// The dedup floor still holds: a re-dispatch of the IDENTICAL attempt (same
/// `{dispatch_key}#{attempt}` key, e.g. a transport-level resend of the same
/// publish) IS suppressed by dedup-on-delivery and returns Err. This is the
/// at-most-once-per-attempt property the per-attempt key preserves — only the
/// attempt boundary opens a fresh delivery, not an arbitrary resend.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_attempt_duplicate_is_still_suppressed_by_dedup() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address();

    let worker = connect_channel(address)?;
    subscribe_worker(&worker)?;
    server.wait_for_connection()?;

    let dispatch = LiminalOutboxDispatch::new(address.to_string(), CHANNEL);
    let row = pending_row(0);

    dispatch
        .dispatch(&row)
        .await
        .map_err(|error| test_error(format!("first dispatch must succeed: {error}")))?;

    // Re-dispatch the EXACT same attempt (no bump): same per-attempt key =>
    // dedup-on-delivery suppresses it => non-accepted ack => Err. The outbox
    // never replays an identical attempt, so this Err cannot mis-drive a retry;
    // it is the redundant idempotency floor the design keeps under aion's terminal
    // dedup authority.
    let duplicate = dispatch.dispatch(&row).await;
    assert!(
        duplicate.is_err(),
        "an identical-attempt duplicate is suppressed by dedup and surfaces as Err"
    );

    server.shutdown()?;
    Ok(())
}

/// A dispatch to a channel with NO subscribed worker returns Err, so the outbox
/// retries rather than recording a false `done` — the honest `Ok` contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_with_no_worker_returns_err() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address();
    // Touch the server so the listener is live, but subscribe no worker.
    let _probe = connect_channel(address)?;
    server.wait_for_connection()?;

    let dispatch = LiminalOutboxDispatch::new(address.to_string(), CHANNEL);
    let row = pending_row(7);
    let result = dispatch.dispatch(&row).await;
    assert!(
        result.is_err(),
        "a dispatch that reaches no worker must return Err so the outbox retries"
    );

    server.shutdown()?;
    Ok(())
}

/// Opens a fresh on-disk `LibSqlStore` for the retry-loop test.
async fn open_libsql_store(name: &str) -> Result<Arc<LibSqlStore>, TestError> {
    let nanos = Instant::now().elapsed().as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aion-13-1-{name}-{}-{nanos}.db",
        std::process::id()
    ));
    LibSqlStore::open(path)
        .await
        .map(Arc::new)
        .map_err(test_error)
}

/// Outbox dispatcher config tuned for a fast, deterministic retry loop.
fn fast_retry_config() -> OutboxDispatcherConfig {
    OutboxDispatcherConfig {
        poll_interval: Duration::from_millis(10),
        batch_size: 16,
        // Generous budget so the loop never dead-letters before the worker joins.
        max_attempts: 100,
        backoff_base: Duration::from_millis(10),
        backoff_multiplier: 1,
        backoff_max: Duration::from_millis(10),
    }
}

/// Polls the outbox row's state until `predicate` holds or the deadline passes.
async fn wait_for_row<F>(
    store: &LibSqlStore,
    dispatch_key: &str,
    deadline: Instant,
    predicate: F,
) -> Result<aion_store_libsql::OutboxRowState, TestError>
where
    F: Fn(&aion_store_libsql::OutboxRowState) -> bool,
{
    while Instant::now() < deadline {
        if let Some(state) = store
            .outbox_row_state(dispatch_key)
            .await
            .map_err(test_error)?
        {
            if predicate(&state) {
                return Ok(state);
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(test_error("outbox row never reached the awaited state"))
}

/// THE 13-1 END-TO-END RETRY TEST: the real `OutboxDispatcher` drives a row over
/// the `LiminalOutboxDispatch` to a liminal server with NO worker. The honest
/// non-accepted ack returns Err, so the row retries with backoff (attempt bumps,
/// stays pending). Once a worker subscribes, the next sweep's bumped-attempt
/// publish is NOT dedup-suppressed (the 13-1 fix) and is genuinely delivered, so
/// the row advances to Done. Proves honest-ack retry over a real liminal loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_dispatcher_retries_over_liminal_then_succeeds_when_worker_joins()
-> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address();

    // Stage one pending row in a real durable outbox store. No worker is
    // subscribed yet, so the first dispatches reach an empty channel.
    let store = open_libsql_store("retry-loop").await?;
    let row = pending_row(0);
    store
        .append_outbox_batch(std::slice::from_ref(&row))
        .await
        .map_err(test_error)?;

    // Run the unchanged outbox dispatcher loop against the liminal transport.
    let dispatch = Arc::new(LiminalOutboxDispatch::new(address.to_string(), CHANNEL));
    let dispatcher = OutboxDispatcher::new(store.clone(), dispatch, fast_retry_config());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let loop_handle = tokio::spawn(dispatcher.run(shutdown_rx));

    // The no-worker dispatches must drive retry/backoff: the row stays pending
    // with a bumped attempt rather than dead-lettering or recording a false done.
    let deadline = Instant::now() + Duration::from_secs(10);
    let retried = wait_for_row(&store, &row.dispatch_key, deadline, |state| {
        state.status == OutboxStatus::Pending && state.attempt >= 1
    })
    .await?;
    assert!(
        retried.attempt >= 1,
        "no-worker dispatch drove at least one honest retry"
    );

    // A worker subscribes; the next sweep's bumped-attempt key is a fresh,
    // non-suppressed publish, so the dispatch is genuinely delivered.
    let worker = connect_channel(address)?;
    subscribe_worker(&worker)?;
    server.wait_for_connection()?;

    let done = wait_for_row(&store, &row.dispatch_key, deadline, |state| {
        state.status == OutboxStatus::Done
    })
    .await?;
    assert_eq!(
        done.status,
        OutboxStatus::Done,
        "the bumped-attempt retry succeeds once a worker is present (not dedup-suppressed)"
    );

    shutdown_tx.send(true).map_err(test_error)?;
    loop_handle.await.map_err(test_error)?;
    server.shutdown()?;
    Ok(())
}

/// THE 13-2 VERIFY: a row re-dispatched through the real reconciler re-arm path
/// (`rearm_stale_claimed_outbox_rows`, which PRESERVES `attempt`) does NOT cause a
/// second worker execution. The re-armed row carries the same `attempt`, so
/// `attempt_idempotency_key` derives the IDENTICAL `{dispatch_key}#{attempt}`
/// liminal idempotency key as the in-flight dispatch; dedup-on-delivery (13-L1)
/// suppresses the duplicate at delivery (`delivered: false` → non-accepted ack →
/// `Err`), so the worker is reached exactly once. This is the 13-2 contract: a
/// retry/reconciler/recovery re-dispatch is deduped, not re-executed.
///
/// Note on the design doc: §5 13-2 says "pass `dispatch_key` as the idempotency
/// key". The BARE stable key would regress 13-1 (every legitimate bumped-attempt
/// retry would be self-suppressed — the documented 13-0 trap). The landed seam
/// instead passes `{dispatch_key}#{attempt}`, which BOTH dedups a same-attempt
/// re-arm (this test) AND lets a real bumped-attempt retry through
/// (`legit_retry_with_bumped_attempt_is_not_suppressed`). 13-2's behavioural
/// guarantee holds because re-arm preserves `attempt`; this test pins exactly that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconciler_rearm_redispatch_is_deduped_no_second_worker_execution() -> Result<(), TestError>
{
    let server = RunningServer::start()?;
    let address = server.address();

    // A worker subscribes so a fresh keyed publish reports a genuine delivery and
    // a duplicate is observably suppressed at the delivery boundary.
    let worker = connect_channel(address)?;
    subscribe_worker(&worker)?;
    server.wait_for_connection()?;

    // Stage one pending row in a real durable outbox store, then CLAIM it so it
    // holds a durable `claimed_at` the reconciler can find as stale.
    let store = open_libsql_store("rearm-dedup").await?;
    let row = pending_row(0);
    store
        .append_outbox_batch(std::slice::from_ref(&row))
        .await
        .map_err(test_error)?;
    let claimed = store.claim_outbox_rows(16).await.map_err(test_error)?;
    let claimed_row = claimed
        .into_iter()
        .find(|r| r.dispatch_key == row.dispatch_key)
        .ok_or_else(|| test_error("claim did not return the staged row"))?;
    assert_eq!(claimed_row.attempt, 0, "first claim is attempt 0");

    // FIRST DISPATCH: the in-flight dispatch the worker is executing. attempt 0,
    // fresh key + live worker => genuine delivery => Ok. This claims the liminal
    // dedup key `{dispatch_key}#0`.
    let dispatch = LiminalOutboxDispatch::new(address.to_string(), CHANNEL);
    dispatch
        .dispatch(&claimed_row)
        .await
        .map_err(|error| test_error(format!("in-flight dispatch must succeed: {error}")))?;

    // RECONCILER RE-ARM: before the completion returns, the reconciler re-arms the
    // stale claimed row. `older_than` is in the future so the just-claimed row
    // qualifies; the re-arm flips it back to pending and PRESERVES attempt 0.
    let future = Utc::now() + chrono::Duration::seconds(60);
    let rearmed = store
        .rearm_stale_claimed_outbox_rows(future, Utc::now(), 16)
        .await
        .map_err(test_error)?;
    let rearmed_row = rearmed
        .into_iter()
        .find(|r| r.dispatch_key == row.dispatch_key)
        .ok_or_else(|| test_error("reconciler did not re-arm the claimed row"))?;
    assert_eq!(
        rearmed_row.attempt, 0,
        "reconciler re-arm preserves attempt, so the idempotency key is unchanged"
    );
    assert_eq!(
        attempt_idempotency_key(&rearmed_row),
        attempt_idempotency_key(&claimed_row),
        "the re-armed row derives the IDENTICAL liminal idempotency key"
    );

    // RE-DISPATCH the re-armed row (re-claimed by the dispatcher's normal sweep).
    // Same `{dispatch_key}#0` key => dedup-on-delivery suppresses the publish
    // (`delivered: false`) => non-accepted ack => Err. The worker is NOT reached a
    // second time: a re-armed duplicate dispatch cannot drive a second execution.
    let reclaimed = store.claim_outbox_rows(16).await.map_err(test_error)?;
    let reclaimed_row = reclaimed
        .into_iter()
        .find(|r| r.dispatch_key == row.dispatch_key)
        .ok_or_else(|| test_error("re-claim did not return the re-armed row"))?;
    assert_eq!(
        reclaimed_row.attempt, 0,
        "re-claim keeps the preserved attempt"
    );
    let redispatch = dispatch.dispatch(&reclaimed_row).await;
    assert!(
        redispatch.is_err(),
        "a reconciler re-armed re-dispatch (same attempt key) is dedup-suppressed, \
         so no second worker execution occurs"
    );

    server.shutdown()?;
    Ok(())
}
