//! Dispatch-outcome reporting and runtime-channel draining for the serve loop.

use std::collections::HashMap;

use aion_core::{ActivityId, WorkflowId};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info};

use crate::context::{ActivityCancellationHandle, HeartbeatRequest};
use crate::error::WorkerError;
use crate::protocol::reconnect::{PendingActivityReport, UnackedResultTracker};
use crate::protocol::{ActivityExecutionKey, HeartbeatBookkeeper, WorkerSession};
use crate::runtime::loop_::DispatchOutcome;

/// Receive halves of the runtime's heartbeat and dispatch-outcome channels.
pub(crate) struct RuntimeChannels {
    /// Heartbeat requests emitted by in-flight activity handlers.
    pub(crate) heartbeats: mpsc::UnboundedReceiver<HeartbeatRequest>,
    /// Completed dispatch outcomes awaiting reporting.
    pub(crate) results: mpsc::UnboundedReceiver<DispatchFinished>,
}

/// Dispatch outcome handed from a spawned activity task back to the loop.
pub(crate) struct DispatchFinished {
    /// Execution key identifying the finished activity.
    pub(crate) key: ActivityExecutionKey,
    /// Outcome computed by the dispatcher, or the dispatch failure.
    pub(crate) outcome: Result<DispatchOutcome, WorkerError>,
}

/// Handles owned by the loop for one in-flight activity.
pub(crate) struct InFlightActivity {
    /// Cooperative cancellation flag shared with the handler context.
    pub(crate) cancellation_handle: ActivityCancellationHandle,
    /// Join handle for the spawned dispatch task.
    pub(crate) join_handle: JoinHandle<()>,
}

/// Awaits remaining in-flight activities and reports their outcomes.
pub(crate) async fn drain_remaining<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    channels: &mut RuntimeChannels,
    in_flight: &mut HashMap<ActivityExecutionKey, InFlightActivity>,
    tracker: &mut UnackedResultTracker,
    tasks_reported: &mut usize,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    while !in_flight.is_empty() {
        match channels.results.recv().await {
            Some(finished) => {
                report_finished(
                    session,
                    heartbeat_bookkeeper,
                    finished,
                    in_flight,
                    tracker,
                    tasks_reported,
                    pending_error,
                )
                .await;
                drain_heartbeats(
                    session,
                    heartbeat_bookkeeper,
                    &mut channels.heartbeats,
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
        &mut channels.heartbeats,
        pending_error,
    )
    .await;
}

/// Forwards queued heartbeats and already-finished outcomes without blocking.
pub(crate) async fn drain_runtime_events<S>(
    session: &mut S,
    heartbeat_bookkeeper: &HeartbeatBookkeeper,
    channels: &mut RuntimeChannels,
    in_flight: &mut HashMap<ActivityExecutionKey, InFlightActivity>,
    tracker: &mut UnackedResultTracker,
    tasks_reported: &mut usize,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    drain_heartbeats(
        session,
        heartbeat_bookkeeper,
        &mut channels.heartbeats,
        pending_error,
    )
    .await;
    while let Ok(finished) = channels.results.try_recv() {
        report_finished(
            session,
            heartbeat_bookkeeper,
            finished,
            in_flight,
            tracker,
            tasks_reported,
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
    tracker: &mut UnackedResultTracker,
    tasks_reported: &mut usize,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    if let Some(in_flight_activity) = in_flight.remove(&finished.key) {
        let _ = in_flight_activity.join_handle.await;
        record_first_error(pending_error, heartbeat_bookkeeper.remove(&finished.key));
    }
    match finished.outcome {
        Ok(outcome) => {
            tracker.record(pending_report(&finished.key, &outcome));
            let sent = report_outcome(
                session,
                finished.key.workflow_id,
                finished.key.activity_id,
                outcome,
            )
            .await;
            if sent.is_ok() {
                // A received task whose outcome report went out is the
                // end-to-end health proof used for drop-budget resets.
                *tasks_reported += 1;
            }
            record_first_error(pending_error, sent);
        }
        Err(error) => {
            if pending_error.is_none() {
                *pending_error = Some(error);
            }
        }
    }
}

/// Builds the unacked-tracker entry for a computed outcome before it is sent.
fn pending_report(key: &ActivityExecutionKey, outcome: &DispatchOutcome) -> PendingActivityReport {
    match outcome {
        DispatchOutcome::Completed { output } => PendingActivityReport::Completed {
            workflow_id: key.workflow_id.clone(),
            activity_id: key.activity_id.clone(),
            output: output.clone(),
        },
        DispatchOutcome::Failed { failure } => PendingActivityReport::Failed {
            workflow_id: key.workflow_id.clone(),
            activity_id: key.activity_id.clone(),
            failure: failure.clone(),
        },
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

fn record_first_error(pending_error: &mut Option<WorkerError>, result: Result<(), WorkerError>) {
    if pending_error.is_none() {
        if let Err(error) = result {
            *pending_error = Some(error);
        }
    }
}
