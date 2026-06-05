//! receive->dispatch->report worker loop + bounded concurrency

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use aion_core::{ActivityError, ActivityId, Payload, WorkflowId};
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info};

use crate::config::WorkerConfig;
use crate::context::{ActivityCancellationHandle, ActivityContext, HeartbeatRequest};
use crate::error::WorkerError;
use crate::protocol::{
    ActivityExecutionKey, ActivityTask, HeartbeatBookkeeper, WorkerSession, WorkerSessionEvent,
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

/// Runs the worker receive loop until the session's task stream completes.
///
/// The loop only forwards explicit handler heartbeats and cancellation flags. It
/// never emits automatic heartbeats, never enforces heartbeat timeouts, and never
/// aborts running handler tasks on cancellation.
///
/// # Errors
///
/// Returns [`WorkerError`] when task decode, dispatch, heartbeat send, or result
/// reporting fails.
pub async fn serve_activity_tasks<S, D>(
    config: &WorkerConfig,
    session: &mut S,
    dispatcher: Arc<D>,
) -> Result<(), WorkerError>
where
    S: WorkerSession,
    D: ActivityDispatcher,
{
    if config.max_concurrency == 0 {
        return Err(WorkerError::registration(InvalidMaxConcurrency));
    }

    let semaphore = Arc::new(Semaphore::new(config.max_concurrency));
    let (result_sender, mut result_receiver) = mpsc::unbounded_channel();
    let (heartbeat_sender, mut heartbeat_receiver) = mpsc::unbounded_channel();
    let heartbeat_bookkeeper = HeartbeatBookkeeper::default();
    let mut stream = session.receive_tasks();
    let mut in_flight = HashMap::<ActivityExecutionKey, InFlightActivity>::new();
    let mut pending_error = None;

    while pending_error.is_none() {
        drain_runtime_events(
            session,
            &heartbeat_bookkeeper,
            &mut heartbeat_receiver,
            &mut result_receiver,
            &mut in_flight,
            &mut pending_error,
        )
        .await;
        if pending_error.is_some() {
            break;
        }

        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(WorkerError::registration)?;
        let Some(event) = stream.next().await else {
            drop(permit);
            break;
        };
        if !handle_session_event(
            event,
            permit,
            Arc::clone(&dispatcher),
            &result_sender,
            &heartbeat_sender,
            &heartbeat_bookkeeper,
            &mut in_flight,
            &mut pending_error,
        )
        .await?
        {
            break;
        }
    }

    drop(result_sender);
    drop(heartbeat_sender);
    drain_remaining(
        session,
        &heartbeat_bookkeeper,
        &mut heartbeat_receiver,
        &mut result_receiver,
        &mut in_flight,
        &mut pending_error,
    )
    .await;

    if let Some(error) = pending_error {
        return Err(error);
    }
    Ok(())
}

async fn handle_session_event<D>(
    event: Result<WorkerSessionEvent, WorkerError>,
    permit: tokio::sync::OwnedSemaphorePermit,
    dispatcher: Arc<D>,
    result_sender: &mpsc::UnboundedSender<DispatchFinished>,
    heartbeat_sender: &mpsc::UnboundedSender<HeartbeatRequest>,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    in_flight: &mut HashMap<ActivityExecutionKey, InFlightActivity>,
    pending_error: &mut Option<WorkerError>,
) -> Result<bool, WorkerError>
where
    D: ActivityDispatcher,
{
    match event {
        Ok(WorkerSessionEvent::Task(proto_task)) => {
            let task = match ActivityTask::try_from(proto_task) {
                Ok(task) => task,
                Err(error) => {
                    drop(permit);
                    *pending_error = Some(error);
                    return Ok(false);
                }
            };
            spawn_activity(
                task,
                permit,
                dispatcher,
                result_sender.clone(),
                heartbeat_sender.clone(),
                heartbeat_bookkeeper,
                in_flight,
            )?;
            Ok(true)
        }
        Ok(WorkerSessionEvent::Cancel {
            workflow_id,
            activity_id,
        }) => {
            drop(permit);
            deliver_cancellation(workflow_id, activity_id, in_flight);
            Ok(true)
        }
        Err(error) => {
            drop(permit);
            *pending_error = Some(error);
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
    heartbeat_bookkeeper.register(task.activity_id.clone())?;
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
    activity_id: ActivityId,
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

async fn drain_remaining<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    heartbeat_receiver: &mut mpsc::UnboundedReceiver<HeartbeatRequest>,
    result_receiver: &mut mpsc::UnboundedReceiver<DispatchFinished>,
    in_flight: &mut HashMap<ActivityExecutionKey, InFlightActivity>,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    while !in_flight.is_empty() {
        match result_receiver.recv().await {
            Some(finished) => {
                report_finished(
                    session,
                    heartbeat_bookkeeper,
                    finished,
                    in_flight,
                    pending_error,
                )
                .await;
                drain_heartbeats(
                    session,
                    heartbeat_bookkeeper,
                    heartbeat_receiver,
                    pending_error,
                )
                .await;
            }
            None => break,
        }
    }
    drain_heartbeats(
        session,
        heartbeat_bookkeeper,
        heartbeat_receiver,
        pending_error,
    )
    .await;
}

async fn drain_runtime_events<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    heartbeat_receiver: &mut mpsc::UnboundedReceiver<HeartbeatRequest>,
    result_receiver: &mut mpsc::UnboundedReceiver<DispatchFinished>,
    in_flight: &mut HashMap<ActivityExecutionKey, InFlightActivity>,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    drain_heartbeats(
        session,
        heartbeat_bookkeeper,
        heartbeat_receiver,
        pending_error,
    )
    .await;
    while let Ok(finished) = result_receiver.try_recv() {
        report_finished(
            session,
            heartbeat_bookkeeper,
            finished,
            in_flight,
            pending_error,
        )
        .await;
    }
}

async fn drain_heartbeats<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    heartbeat_receiver: &mut mpsc::UnboundedReceiver<HeartbeatRequest>,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    while let Ok(request) = heartbeat_receiver.try_recv() {
        record_first_error(
            pending_error,
            crate::protocol::send_heartbeat(session, heartbeat_bookkeeper, request).await,
        );
    }
}

async fn report_finished<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    finished: DispatchFinished,
    in_flight: &mut HashMap<ActivityExecutionKey, InFlightActivity>,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    if let Some(in_flight_activity) = in_flight.remove(&finished.key) {
        let _ = in_flight_activity.join_handle.await;
        record_first_error(
            pending_error,
            heartbeat_bookkeeper.remove(&finished.key.activity_id),
        );
    }
    match finished.outcome {
        Ok(outcome) => record_first_error(
            pending_error,
            report_outcome(
                session,
                finished.key.workflow_id,
                finished.key.activity_id,
                outcome,
            )
            .await,
        ),
        Err(error) => {
            if pending_error.is_none() {
                *pending_error = Some(error);
            }
        }
    }
}

async fn report_outcome<S>(
    session: &mut S,
    workflow_id: WorkflowId,
    activity_id: ActivityId,
    outcome: DispatchOutcome,
) -> Result<(), WorkerError>
where
    S: WorkerSession,
{
    debug!(
        activity_id = activity_id.sequence_position(),
        "reporting activity outcome"
    );
    match outcome {
        DispatchOutcome::Completed { output } => {
            session
                .report_result(workflow_id, activity_id.clone(), output)
                .await?;
            info!(
                activity_id = activity_id.sequence_position(),
                "reported activity result"
            );
        }
        DispatchOutcome::Failed { failure } => {
            session
                .report_failure(workflow_id, activity_id.clone(), failure)
                .await?;
            info!(
                activity_id = activity_id.sequence_position(),
                "reported activity failure"
            );
        }
    }
    Ok(())
}

struct DispatchFinished {
    key: ActivityExecutionKey,
    outcome: Result<DispatchOutcome, WorkerError>,
}

struct InFlightActivity {
    cancellation_handle: ActivityCancellationHandle,
    join_handle: JoinHandle<()>,
}

fn record_first_error(pending_error: &mut Option<WorkerError>, result: Result<(), WorkerError>) {
    if pending_error.is_none() {
        if let Err(error) = result {
            *pending_error = Some(error);
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("worker max_concurrency must be greater than zero")]
struct InvalidMaxConcurrency;

#[cfg(test)]
#[path = "loop_tests.rs"]
mod tests;
