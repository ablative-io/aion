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
    DispatchFinished, InFlightActivity, RuntimeChannels, drain_remaining, drain_runtime_events,
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
    /// The server ended the task stream cleanly (end-of-stream or a drain
    /// frame). The reconnect-aware run loop treats this as a retryable
    /// session drop — never as a run end — so workers ride through graceful
    /// server closes such as deploys.
    StreamClosed,
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
}

/// Runs the worker receive loop until the session's task stream completes.
///
/// The loop only forwards explicit handler heartbeats and cancellation flags. It
/// never emits automatic heartbeats, never enforces heartbeat timeouts, and never
/// aborts running handler tasks on cancellation.
///
/// Every computed dispatch outcome is recorded in `tracker` before its report
/// is sent, so a caller that reconnects after a transport drop can re-report
/// the backlog; the engine ingests reports idempotently by `ActivityId`.
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
/// the backlog; the engine ingests reports idempotently by `ActivityId`. Only
/// an explicit engine acknowledgement clears tracker entries, so successful
/// sends leave their entries in place.
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
    if config.max_concurrency == 0 {
        return Err(WorkerError::registration(InvalidMaxConcurrency));
    }

    let semaphore = Arc::new(Semaphore::new(config.max_concurrency));
    let (result_sender, result_receiver) = mpsc::unbounded_channel();
    let (heartbeat_sender, heartbeat_receiver) = mpsc::unbounded_channel();
    let mut channels = RuntimeChannels {
        heartbeats: heartbeat_receiver,
        results: result_receiver,
    };
    let heartbeat_bookkeeper = HeartbeatBookkeeper::default();
    let mut stream = session.receive_tasks();
    let mut in_flight = HashMap::<ActivityExecutionKey, InFlightActivity>::new();
    let mut pending_error = None;
    // Overridden at the shutdown break sites; every other clean exit is the
    // server ending the stream.
    let mut end = ServeEnd::StreamClosed;
    tokio::pin!(shutdown);

    while pending_error.is_none() {
        drain_runtime_events(
            session,
            &heartbeat_bookkeeper,
            &mut channels,
            &mut in_flight,
            tracker,
            &mut health.tasks_reported,
            &mut pending_error,
        )
        .await;
        if pending_error.is_some() {
            break;
        }

        tokio::select! {
            biased;
            () = &mut shutdown => {
                cancel_all_in_flight(&in_flight);
                end = ServeEnd::Shutdown;
                break;
            }
            event = stream.next() => {
                let Some(event) = event else { break; };
                match event {
                    Ok(WorkerSessionEvent::Cancel { workflow_id, activity_id }) => {
                        deliver_cancellation(workflow_id, &activity_id, &in_flight);
                    }
                    Ok(WorkerSessionEvent::Drain) => {
                        break;
                    }
                    other => {
                        let permit = tokio::select! {
                            biased;
                            () = &mut shutdown => {
                                cancel_all_in_flight(&in_flight);
                                end = ServeEnd::Shutdown;
                                break;
                            }
                            permit = semaphore.clone().acquire_owned() => {
                                permit.map_err(WorkerError::registration)?
                            }
                        };
                        if !handle_session_event(
                            other,
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

    drop(result_sender);
    drop(heartbeat_sender);
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

    if let Some(error) = pending_error {
        return Err(error);
    }
    Ok(end)
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

fn handle_session_event<D>(
    event: Result<WorkerSessionEvent, WorkerError>,
    ctx: SessionEventContext<'_, D>,
) -> Result<bool, WorkerError>
where
    D: ActivityDispatcher,
{
    match event {
        Ok(WorkerSessionEvent::Task(proto_task)) => {
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
        Ok(WorkerSessionEvent::Cancel { .. } | WorkerSessionEvent::Drain) => {
            drop(ctx.permit);
            Ok(true)
        }
        Err(error) => {
            drop(ctx.permit);
            *ctx.pending_error = Some(error);
            Ok(false)
        }
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
