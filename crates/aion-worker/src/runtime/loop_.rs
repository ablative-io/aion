//! receive->dispatch->report worker loop + bounded concurrency

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use aion_core::{ActivityError, ActivityId, Payload, WorkflowId};
use async_trait::async_trait;
use futures::StreamExt;
use futures::future;
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info};

use crate::config::WorkerConfig;
use crate::context::{ActivityContext, HeartbeatRequest};
use crate::error::WorkerError;
use crate::protocol::reconnect::UnackedResultTracker;
use crate::protocol::{
    ActivityExecutionKey, ActivityTask, HeartbeatBookkeeper, WorkerSession, WorkerSessionEvent,
};
use crate::runtime::report::{
    DispatchFinished, InFlightActivity, RuntimeChannels, drain_remaining, record_first_error,
    report_finished,
};

/// Dispatch seam used by the receive loop to execute decoded activity tasks.
#[async_trait]
pub trait ActivityDispatcher: Send + Sync + 'static {
    /// Executes one decoded activity task with the provided handler context.
    async fn dispatch(
        &self,
        task: ActivityTask,
        context: ActivityContext,
    ) -> Result<DispatchOutcome, WorkerError>;

    /// Activity type names this dispatcher can serve.
    fn activity_types(&self) -> BTreeSet<String>;
}

/// Activity execution outcome returned by the dispatch seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Activity completed with an output payload.
    Completed {
        /// Opaque output payload.
        output: Payload,
    },
    /// Activity failed with explicit classification.
    Failed {
        /// Classified activity failure.
        failure: ActivityError,
    },
}

/// Future that never resolves, used by the default serve entrypoint.
pub type NoShutdown = future::Pending<()>;

/// Why the serve loop ended without an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeEnd {
    /// The caller's shutdown future fired; in-flight work was drained.
    Shutdown,
    /// The server ended the task stream cleanly without announcing a drain.
    /// The reconnect-aware run loop treats this unannounced close as a
    /// budgeted retryable session drop — never as a run end.
    StreamClosed,
    /// The server announced a drain: in-flight work was finished and
    /// reported, and the run loop reconnects after the schedule's initial
    /// backoff without consuming any drop budget.
    Drained,
}

/// Per-session health accounting written by the serve loop for the
/// reconnect-aware caller's drop-budget reset decision.
#[derive(Debug, Default)]
pub struct SessionHealth {
    /// Activity tasks whose outcome report was sent on this session.
    pub tasks_reported: usize,
    /// When the receive stream ended or dropped, captured before in-flight
    /// handlers are drained — so post-drop draining never extends the
    /// session's measured connected lifetime.
    pub stream_ended_at: Option<tokio::time::Instant>,
    /// Latched when a drain frame is observed on this session: the eventual
    /// stream end — clean OR abrupt — is then drain-class (the server
    /// announced it was going away), so the drop consumes no budget even if
    /// the post-drain reporting fails. Survives an error return because this
    /// is an out-parameter.
    pub drain_received: bool,
}

/// Runs the worker receive loop until the session's task stream completes.
///
/// The RUNTIME owns liveness: for a session that carries a server-assigned
/// heartbeat window ([`WorkerSession::heartbeat_window`]), the loop
/// automatically heartbeats every in-flight activity at a quarter-window
/// cadence, so a healthy worker running a legitimately long activity is never
/// expired by the server's heartbeat sweeper. Explicit handler heartbeats
/// remain the way to attach PROGRESS payloads; they are forwarded as they
/// arrive. The loop never enforces heartbeat timeouts locally and never
/// aborts running handler tasks on cancellation.
///
/// Every computed dispatch outcome is recorded in `tracker` before its report
/// is sent, so a caller that reconnects after a transport drop can re-report
/// the backlog; the server acks each consumed report (`ResultAck`), and only
/// that explicit acknowledgement clears a tracker entry.
///
/// # Errors
///
/// Returns [`WorkerError`] when task decode, dispatch, heartbeat send, or result
/// reporting fails.
pub async fn serve_activity_tasks<S, D>(
    config: &WorkerConfig,
    session: &mut S,
    dispatcher: Arc<D>,
    tracker: &mut UnackedResultTracker,
) -> Result<ServeEnd, WorkerError>
where
    S: WorkerSession,
    D: ActivityDispatcher,
{
    let mut health = SessionHealth::default();
    serve_activity_tasks_until(
        config,
        session,
        dispatcher,
        tracker,
        &mut health,
        future::pending(),
    )
    .await
}

/// Runs the worker receive loop until the session's task stream completes.
///
/// The RUNTIME owns liveness (#176): when the session carries a
/// server-assigned heartbeat window ([`WorkerSession::heartbeat_window`],
/// from the `RegisterAck`), the loop automatically sends a liveness heartbeat
/// for EVERY in-flight activity at a quarter-window cadence
/// ([`liveness_pump_interval`]). The server's heartbeat sweeper expires any
/// worker whose in-flight task exceeds the window without a heartbeat — that
/// is dead/wedged-PROCESS detection, and a healthy process running a
/// multi-minute handler must never trip it, so keeping tasks beating is the
/// runtime's job, not each handler's. A wedged process (deadlocked loop,
/// stopped host) stops pumping and is correctly expired. Explicit handler
/// heartbeats remain the way to attach PROGRESS payloads and are forwarded as
/// they arrive; the loop never enforces heartbeat timeouts locally and never
/// aborts running handler tasks on cancellation.
///
/// Every computed dispatch outcome is recorded in `tracker` before its report
/// is sent, so a caller that reconnects after a transport drop can re-report
/// the backlog; the server ingests reports idempotently and acks each one
/// with a `ResultAck` frame. Only that explicit acknowledgement clears a
/// tracker entry — a successful send proves nothing on its own.
///
/// `health` accumulates session-health accounting: the activity tasks whose
/// outcome report was sent on this session, and the instant the receive
/// stream ended (captured before in-flight handlers are drained). It is an
/// out-parameter (rather than part of the return value) so the accounting
/// survives an error return: the reconnect-aware caller uses it for the
/// drop-budget reset decision — a session that served at least one task, or
/// that stayed connected longer than the maximum backoff delay measured to
/// the recorded stream end (never to the end of the post-drop drain), resets
/// the cumulative drop budget even when it later drops.
///
/// On a clean end this returns [`ServeEnd`] distinguishing a caller-driven
/// shutdown from a server-side stream close, so the caller can treat the
/// latter as a retryable drop.
///
/// # Errors
///
/// Returns [`WorkerError`] when task decode, dispatch, heartbeat send, or result
/// reporting fails.
pub async fn serve_activity_tasks_until<S, D, Shutdown>(
    config: &WorkerConfig,
    session: &mut S,
    dispatcher: Arc<D>,
    tracker: &mut UnackedResultTracker,
    health: &mut SessionHealth,
    shutdown: Shutdown,
) -> Result<ServeEnd, WorkerError>
where
    S: WorkerSession,
    D: ActivityDispatcher,
    Shutdown: Future<Output = ()> + Send,
{
    ensure_max_concurrency(config)?;
    let semaphore = Arc::new(Semaphore::new(config.max_concurrency));
    let (result_sender, heartbeat_sender, mut channels) = runtime_channels();
    let heartbeat_bookkeeper = HeartbeatBookkeeper::default();
    let mut liveness_pump = liveness_pump_for(session);
    let mut stream = session.receive_tasks();
    let mut in_flight = HashMap::<ActivityExecutionKey, InFlightActivity>::new();
    let mut pending_error = None;
    // Overridden at the shutdown break sites; every other clean exit is the
    // server ending the stream.
    let mut end = ServeEnd::StreamClosed;
    tokio::pin!(shutdown);

    // No batching preamble: the select arms below consume queued dispatch
    // outcomes and heartbeats directly, so nothing waits for a stream event.
    while pending_error.is_none() {
        tokio::select! {
            biased;
            () = &mut shutdown => {
                cancel_all_in_flight(&in_flight);
                end = ServeEnd::Shutdown;
                break;
            }
            // Dispatch outcomes are reported the moment they complete — the
            // loop must not sit in `stream.next()` while a finished result
            // waits, or a single dispatched task on an otherwise idle stream
            // is only reported when the stream ends (the server-side dispatch
            // would time out against a healthy worker).
            finished = channels.results.recv() => {
                consume_finished(
                    session,
                    &heartbeat_bookkeeper,
                    finished,
                    &mut in_flight,
                    tracker,
                    health,
                    &mut pending_error,
                )
                .await;
            }
            // Handler heartbeats are forwarded as they arrive for the same
            // reason: the server's liveness window must be beatable while the
            // stream is idle.
            request = channels.heartbeats.recv() => {
                forward_heartbeat(session, &heartbeat_bookkeeper, request, &mut pending_error)
                    .await;
            }
            // Automatic liveness beats for every in-flight activity: the
            // runtime — not each handler — keeps the server's per-task
            // heartbeat window satisfied while a handler legitimately runs
            // longer than the window. Disabled while nothing is in flight
            // (an idle worker has no tracked task to keep alive).
            () = tick_liveness_pump(&mut liveness_pump), if !in_flight.is_empty() => {
                pump_liveness(session, &heartbeat_bookkeeper, &in_flight, &mut pending_error)
                    .await;
            }
            event = stream.next() => {
                let Some(event) = event else { break; };
                match event {
                    Ok(WorkerSessionEvent::Cancel { workflow_id, activity_id }) => {
                        deliver_cancellation(workflow_id, &activity_id, &in_flight);
                    }
                    // Acks are bookkeeping, not work: consumed without a
                    // concurrency permit, like cancellation delivery.
                    Ok(WorkerSessionEvent::ResultAck { workflow_id, activity_id }) => {
                        acknowledge_result(&workflow_id, &activity_id, tracker);
                    }
                    Ok(WorkerSessionEvent::Drain) => {
                        info!("server drain received; finishing in-flight work before reconnect");
                        health.drain_received = true;
                        end = ServeEnd::Drained;
                        break;
                    }
                    Err(error) => {
                        pending_error = Some(error);
                        break;
                    }
                    Ok(WorkerSessionEvent::Task(proto_task)) => {
                        let Some(permit) =
                            acquire_permit_or_shutdown(shutdown.as_mut(), &semaphore).await?
                        else {
                            cancel_all_in_flight(&in_flight);
                            end = ServeEnd::Shutdown;
                            break;
                        };
                        if !handle_task(
                            proto_task,
                            SessionEventContext {
                                permit,
                                dispatcher: Arc::clone(&dispatcher),
                                result_sender: &result_sender,
                                heartbeat_sender: &heartbeat_sender,
                                heartbeat_bookkeeper: &heartbeat_bookkeeper,
                                in_flight: &mut in_flight,
                                pending_error: &mut pending_error,
                            },
                        )? {
                            break;
                        }
                    }
                }
            }
        }
    }

    // The stream just ended — cleanly, by error, or by shutdown. Capture the
    // moment before draining in-flight handlers so the caller's drop-budget
    // reset decision measures connected time, never drain time.
    health.stream_ended_at = Some(tokio::time::Instant::now());

    drop((result_sender, heartbeat_sender));
    drain_remaining(
        session,
        &heartbeat_bookkeeper,
        &mut channels,
        &mut in_flight,
        tracker,
        &mut health.tasks_reported,
        &mut pending_error,
    )
    .await;

    pending_error.map_or(Ok(end), Err)
}

/// Builds the runtime's dispatch-outcome and heartbeat channels.
fn runtime_channels() -> (
    mpsc::UnboundedSender<DispatchFinished>,
    mpsc::UnboundedSender<HeartbeatRequest>,
    RuntimeChannels,
) {
    let (result_sender, result_receiver) = mpsc::unbounded_channel();
    let (heartbeat_sender, heartbeat_receiver) = mpsc::unbounded_channel();
    let channels = RuntimeChannels {
        heartbeats: heartbeat_receiver,
        results: result_receiver,
    };
    (result_sender, heartbeat_sender, channels)
}

struct SessionEventContext<'a, D> {
    permit: tokio::sync::OwnedSemaphorePermit,
    dispatcher: Arc<D>,
    result_sender: &'a mpsc::UnboundedSender<DispatchFinished>,
    heartbeat_sender: &'a mpsc::UnboundedSender<HeartbeatRequest>,
    heartbeat_bookkeeper: &'a HeartbeatBookkeeper,
    in_flight: &'a mut HashMap<ActivityExecutionKey, InFlightActivity>,
    pending_error: &'a mut Option<WorkerError>,
}

fn handle_task<D>(
    proto_task: aion_proto::ProtoActivityTask,
    ctx: SessionEventContext<'_, D>,
) -> Result<bool, WorkerError>
where
    D: ActivityDispatcher,
{
    let task = match ActivityTask::try_from(proto_task) {
        Ok(task) => task,
        Err(error) => {
            drop(ctx.permit);
            *ctx.pending_error = Some(error);
            return Ok(false);
        }
    };
    spawn_activity(
        task,
        ctx.permit,
        ctx.dispatcher,
        ctx.result_sender.clone(),
        ctx.heartbeat_sender.clone(),
        ctx.heartbeat_bookkeeper,
        ctx.in_flight,
    )?;
    Ok(true)
}

/// Rejects a zero `max_concurrency` before the serve loop starts.
fn ensure_max_concurrency(config: &WorkerConfig) -> Result<(), WorkerError> {
    if config.max_concurrency == 0 {
        return Err(WorkerError::registration(InvalidMaxConcurrency));
    }
    Ok(())
}

/// Waits for a dispatch permit, racing the caller's shutdown future; returns
/// `None` when shutdown won.
async fn acquire_permit_or_shutdown<F>(
    shutdown: std::pin::Pin<&mut F>,
    semaphore: &Arc<Semaphore>,
) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, WorkerError>
where
    F: Future<Output = ()> + Send,
{
    tokio::select! {
        biased;
        () = shutdown => Ok(None),
        permit = Arc::clone(semaphore).acquire_owned() => {
            permit.map(Some).map_err(WorkerError::registration)
        }
    }
}

/// Build the automatic liveness pump for a session: sessions registered
/// against a server heartbeat window ([`WorkerSession::heartbeat_window`])
/// beat every in-flight activity at a quarter-window cadence so the server's
/// expiry sweeper only ever fires on a genuinely dead/wedged process.
/// Sessions without a window (fakes, tests) never pump — byte-identical to
/// the pre-pump loop.
fn liveness_pump_for<S>(session: &S) -> Option<tokio::time::Interval>
where
    S: WorkerSession,
{
    session.heartbeat_window().map(|window| {
        let mut ticks = tokio::time::interval(liveness_pump_interval(window));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks
    })
}

/// Consume one queued dispatch outcome (a `None` channel read is a no-op)
/// and report it through the session, mirroring the drain path's
/// [`report_finished`].
async fn consume_finished<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    finished: Option<DispatchFinished>,
    in_flight: &mut HashMap<ActivityExecutionKey, InFlightActivity>,
    tracker: &mut UnackedResultTracker,
    health: &mut SessionHealth,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    if let Some(finished) = finished {
        report_finished(
            session,
            heartbeat_bookkeeper,
            finished,
            in_flight,
            tracker,
            &mut health.tasks_reported,
            pending_error,
        )
        .await;
    }
}

/// Automatic liveness-heartbeat cadence derived from the server-assigned
/// heartbeat window: a quarter of the window, floored at one millisecond
/// (`tokio::time::interval` rejects a zero period).
///
/// The server expires a task once it goes longer than the WHOLE window
/// without a heartbeat, so a quarter-window pump gives roughly four beats per
/// window — comfortably inside the contract even when an individual beat is
/// delayed by a busy loop iteration. Deliberately derived rather than
/// configurable: the window is the server operator's contract, and the pump
/// cadence is an implementation detail of honouring it (mirroring the
/// server's own derived sweep cadence).
#[must_use]
pub(crate) fn liveness_pump_interval(heartbeat_window: std::time::Duration) -> std::time::Duration {
    (heartbeat_window / 4).max(std::time::Duration::from_millis(1))
}

/// Resolves on the next automatic liveness tick, or never for sessions
/// without a server-assigned heartbeat window (fakes and unregistered
/// sessions never pump).
async fn tick_liveness_pump(pump: &mut Option<tokio::time::Interval>) {
    match pump {
        Some(ticks) => {
            ticks.tick().await;
        }
        None => future::pending().await,
    }
}

/// Sends one automatic liveness heartbeat (no progress payload) for every
/// in-flight activity, recording the first send error. A liveness beat never
/// carries progress — explicit handler heartbeats own the progress channel.
async fn pump_liveness<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    in_flight: &HashMap<ActivityExecutionKey, InFlightActivity>,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    for key in in_flight.keys() {
        record_first_error(
            pending_error,
            crate::protocol::send_heartbeat(
                session,
                heartbeat_bookkeeper,
                HeartbeatRequest {
                    workflow_id: key.workflow_id.clone(),
                    activity_id: key.activity_id.clone(),
                    detail: None,
                },
            )
            .await,
        );
        if pending_error.is_some() {
            // The session send path is broken; the loop is about to exit
            // with this error, so further beats are pointless.
            return;
        }
    }
}

/// Forwards one queued handler heartbeat (a `None` channel read is a no-op)
/// to the session, recording the first error.
async fn forward_heartbeat<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    request: Option<HeartbeatRequest>,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    if let Some(request) = request {
        record_first_error(
            pending_error,
            crate::protocol::send_heartbeat(session, heartbeat_bookkeeper, request).await,
        );
    }
}

/// Clears the acknowledged tracker entry; an unknown ack (already cleared on
/// a previous session, or replaced by a re-record) is a logged no-op.
fn acknowledge_result(
    workflow_id: &WorkflowId,
    activity_id: &ActivityId,
    tracker: &mut UnackedResultTracker,
) {
    if tracker.acknowledge(workflow_id, activity_id).is_some() {
        debug!(
            workflow_id = %workflow_id,
            activity_id = activity_id.sequence_position(),
            "server acknowledged activity result; tracker entry cleared"
        );
    } else {
        debug!(
            workflow_id = %workflow_id,
            activity_id = activity_id.sequence_position(),
            "result ack for unknown tracker entry ignored"
        );
    }
}

/// Render an activity's display labels as a compact, log-friendly
/// `key=value` list in stable key order (for example `brief=IP-001
/// repo=ablative-io/yggdrasil`). Empty when the workflow attached none.
fn render_labels(labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn spawn_activity<D>(
    task: ActivityTask,
    permit: tokio::sync::OwnedSemaphorePermit,
    dispatcher: Arc<D>,
    result_sender: mpsc::UnboundedSender<DispatchFinished>,
    heartbeat_sender: mpsc::UnboundedSender<HeartbeatRequest>,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    in_flight: &mut HashMap<ActivityExecutionKey, InFlightActivity>,
) -> Result<(), WorkerError>
where
    D: ActivityDispatcher,
{
    info!(
        activity_type = %task.activity_type,
        activity_id = task.activity_id.sequence_position(),
        workflow_id = %task.workflow_id,
        attempt = task.attempt,
        labels = %render_labels(&task.labels),
        "received activity task"
    );
    let key = ActivityExecutionKey::new(task.workflow_id.clone(), task.activity_id.clone());
    heartbeat_bookkeeper.register(key.clone())?;
    let (context, cancellation_handle) = ActivityContext::for_workflow(
        Some(task.workflow_id.clone()),
        task.activity_id.clone(),
        task.attempt,
        Some(heartbeat_sender),
    );
    let finished_key = key.clone();
    let finished_run_id = task.run_id.clone();
    let join_handle = tokio::spawn(async move {
        let outcome = dispatcher.dispatch(task, context).await;
        if result_sender
            .send(DispatchFinished {
                key: finished_key,
                run_id: finished_run_id,
                outcome,
            })
            .is_err()
        {
            debug!("worker loop stopped before dispatch outcome could be delivered");
        }
        drop(permit);
    });
    in_flight.insert(
        key,
        InFlightActivity {
            cancellation_handle,
            join_handle,
        },
    );
    Ok(())
}

fn deliver_cancellation(
    workflow_id: WorkflowId,
    activity_id: &ActivityId,
    in_flight: &HashMap<ActivityExecutionKey, InFlightActivity>,
) {
    let key = ActivityExecutionKey::new(workflow_id, activity_id.clone());
    if let Some(in_flight_activity) = in_flight.get(&key) {
        in_flight_activity.cancellation_handle.cancel();
        info!(
            activity_id = activity_id.sequence_position(),
            "delivered cooperative activity cancellation"
        );
    }
}

fn cancel_all_in_flight(in_flight: &HashMap<ActivityExecutionKey, InFlightActivity>) {
    for (key, in_flight_activity) in in_flight {
        in_flight_activity.cancellation_handle.cancel();
        info!(
            activity_id = key.activity_id.sequence_position(),
            workflow_id = %key.workflow_id,
            "delivered cooperative activity cancellation during worker shutdown"
        );
    }
}

#[derive(Debug, thiserror::Error)]
#[error("worker max_concurrency must be greater than zero")]
struct InvalidMaxConcurrency;

#[cfg(test)]
#[path = "loop_tests.rs"]
mod tests;
