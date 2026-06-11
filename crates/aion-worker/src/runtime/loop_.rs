//! receive->dispatch->report worker loop + bounded concurrency

use std::collections::{BTreeSet, HashMap};
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
/// The loop only forwards explicit handler heartbeats and cancellation flags. It
/// never emits automatic heartbeats, never enforces heartbeat timeouts, and never
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
/// The loop only forwards explicit handler heartbeats and cancellation flags. It
/// never emits automatic heartbeats, never enforces heartbeat timeouts, and never
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
                if let Some(finished) = finished {
                    report_finished(
                        session,
                        &heartbeat_bookkeeper,
                        finished,
                        &mut in_flight,
                        tracker,
                        &mut health.tasks_reported,
                        &mut pending_error,
                    )
                    .await;
                }
            }
            // Handler heartbeats are forwarded as they arrive for the same
            // reason: the server's liveness window must be beatable while the
            // stream is idle.
            request = channels.heartbeats.recv() => {
                if let Some(request) = request {
                    forward_heartbeat(session, &heartbeat_bookkeeper, request, &mut pending_error)
                        .await;
                }
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

/// Forwards one handler heartbeat to the session, recording the first error.
async fn forward_heartbeat<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    request: HeartbeatRequest,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    record_first_error(
        pending_error,
        crate::protocol::send_heartbeat(session, heartbeat_bookkeeper, request).await,
    );
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
    let join_handle = tokio::spawn(async move {
        let outcome = dispatcher.dispatch(task, context).await;
        if result_sender
            .send(DispatchFinished {
                key: finished_key,
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
