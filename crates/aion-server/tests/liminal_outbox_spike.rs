//! #13-0 spike: one outbox fan-out dispatch over liminal, result back through
//! the existing `OutboxDeliveryCallback`, behind the `liminal-transport` feature.
//!
//! The whole file is gated on the feature, so a default build never compiles it
//! and never links liminal. It stands up a REAL `liminal-server` over loopback
//! TCP, drives `LiminalOutboxDispatch::dispatch` (the dispatch-out seam) across
//! the socket, and routes a worker result back through `LiminalCompletionSource`
//! into a recording `OutboxDeliveryCallback` (the completion-return seam) — the
//! same callback trait the gRPC completion path uses.
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
};
use aion_server::worker::{OutboxDeliveryCallback, OutboxRowDispatch};
use aion_store::{OutboxRow, OutboxStatus};
use chrono::Utc;
use futures::Stream;
use liminal_sdk::{
    ChannelHandle, ConnectionPoolConfig, ConversationHandle, RemoteChannelHandle, RemoteConfig,
    RemoteConversationHandle,
};
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::ConnectionSupervisor;
use liminal_server::server::listener::ServerListener;
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

/// THE DEDUP<->RETRY COMPOSITION TEST: a re-dispatch of the SAME `dispatch_key`
/// (the stable key the outbox reuses on every retry) is suppressed by liminal's
/// dedup-on-delivery, so `LiminalOutboxDispatch::dispatch` returns Err on the
/// second call EVEN THOUGH a worker is present. This documents the known trap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redispatch_with_stable_key_is_suppressed_by_dedup() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let address = server.address();

    let worker = connect_channel(address)?;
    subscribe_worker(&worker)?;
    server.wait_for_connection()?;

    let dispatch = LiminalOutboxDispatch::new(address.to_string(), CHANNEL);
    let row = pending_row(0);

    // First dispatch: fresh key + live worker => genuine delivery => Ok.
    dispatch
        .dispatch(&row)
        .await
        .map_err(|error| test_error(format!("first dispatch must succeed: {error}")))?;

    // Re-dispatch the SAME row (same stable dispatch_key, as a retry/re-arm
    // would). liminal dedup-on-delivery suppresses the second delivery, so the
    // ack is non-accepted and dispatch returns Err — INDISTINGUISHABLE from
    // "reached no worker". A naive outbox would treat this legitimate retry as a
    // hard failure and burn an attempt / dead-letter the row.
    let second = dispatch.dispatch(&row).await;
    assert!(
        second.is_err(),
        "re-dispatch with the stable dispatch_key is suppressed by dedup and surfaces as Err"
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
