//! receive->dispatch->report worker loop + bounded concurrency

use std::collections::BTreeSet;
use std::sync::Arc;

use aion_core::{ActivityError, ActivityId, Payload, WorkflowId};
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tracing::{debug, info, warn};

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::protocol::{
    ActivityTask, PendingActivityReport, UnackedResultTracker, WorkerSession, re_report_unacked,
    reconnect_with_backoff,
};

/// Dispatch seam used by the receive loop to execute decoded activity tasks.
///
/// AR-003 fills this seam with typed handler invocation and failure
/// classification. This loop only owns task intake, bounded concurrency, and
/// outcome reporting.
#[async_trait]
pub trait ActivityDispatcher: Send + Sync + 'static {
    /// Executes one decoded activity task and returns the outcome to report.
    async fn dispatch(&self, task: ActivityTask) -> Result<DispatchOutcome, WorkerError>;

    /// Activity type names this dispatcher can serve.
    fn activity_types(&self) -> BTreeSet<String>;
}

/// Activity execution outcome returned by the dispatch seam.
///
/// Correlation identifiers are deliberately not part of this outcome: the loop
/// reports using the [`ActivityTask`] ids it decoded from the wire so a
/// dispatcher cannot accidentally report an outcome for the wrong task.
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
/// The loop acquires execution capacity before polling the receive stream so a
/// saturated worker applies backpressure to the transport instead of buffering
/// tasks unboundedly. Once intake closes, in-flight dispatches are drained and
/// reported before returning.
///
/// # Errors
///
/// Returns [`WorkerError`] when task decode, dispatch, or result reporting fails.
pub async fn serve_activity_tasks<S, D>(
    config: &WorkerConfig,
    session: &mut S,
    dispatcher: Arc<D>,
) -> Result<(), WorkerError>
where
    S: WorkerSession,
    D: ActivityDispatcher,
{
    let mut tracker = UnackedResultTracker::new();
    serve_activity_tasks_with_tracker(config, session, dispatcher, &mut tracker).await
}

/// Runs the worker receive loop using the supplied un-acked result tracker.
///
/// # Errors
///
/// Returns [`WorkerError`] when task decode, dispatch, or result reporting fails.
/// Runs serving across reconnects, re-registering and re-reporting backlog first.
///
/// # Errors
///
/// Returns [`WorkerError`] when serving or reconnecting fails permanently.
pub async fn serve_activity_tasks_with_reconnect<S, D, F, Fut>(
    config: &WorkerConfig,
    mut session: S,
    dispatcher: Arc<D>,
    mut connect: F,
) -> Result<(), WorkerError>
where
    S: WorkerSession,
    D: ActivityDispatcher,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<S, WorkerError>>,
{
    let mut tracker = UnackedResultTracker::new();
    let available_handlers = dispatcher.activity_types();
    let activity_types = available_handlers.iter().cloned().collect::<Vec<_>>();

    loop {
        re_report_unacked(&tracker, &mut session).await?;
        match serve_activity_tasks_with_tracker(
            config,
            &mut session,
            Arc::clone(&dispatcher),
            &mut tracker,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) => warn!(error = %error, "worker session dropped; reconnecting"),
        }
        session = reconnect_with_backoff(
            config,
            activity_types.clone(),
            &available_handlers,
            &mut connect,
        )
        .await?;
    }
}

/// Runs the worker receive loop using a caller-owned un-acked result tracker.
///
/// # Errors
///
/// Returns [`WorkerError`] when task decode, dispatch, or result reporting fails.
pub async fn serve_activity_tasks_with_tracker<S, D>(
    config: &WorkerConfig,
    session: &mut S,
    dispatcher: Arc<D>,
    tracker: &mut UnackedResultTracker,
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
    let mut stream = session.receive_tasks();
    let mut in_flight = 0usize;
    let mut intake_open = true;
    let mut pending_error = None;

    while intake_open {
        drain_finished_reports(
            session,
            tracker,
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
        drain_finished_reports(
            session,
            tracker,
            &mut result_receiver,
            &mut in_flight,
            &mut pending_error,
        )
        .await;
        if pending_error.is_some() {
            drop(permit);
            break;
        }

        match stream.next().await {
            Some(Ok(proto_task)) => {
                let task = match ActivityTask::try_from(proto_task) {
                    Ok(task) => task,
                    Err(error) => {
                        drop(permit);
                        pending_error = Some(error);
                        intake_open = false;
                        continue;
                    }
                };
                spawn_dispatch(task, Arc::clone(&dispatcher), result_sender.clone(), permit);
                in_flight += 1;
            }
            Some(Err(error)) => {
                drop(permit);
                pending_error = Some(error);
                intake_open = false;
            }
            None => {
                drop(permit);
                intake_open = false;
            }
        }
    }

    drop(result_sender);
    while in_flight > 0 {
        match result_receiver.recv().await {
            Some(finished) => {
                report_finished(session, tracker, finished, &mut pending_error).await;
                in_flight = in_flight.saturating_sub(1);
            }
            None => break,
        }
    }

    if let Some(error) = pending_error {
        return Err(error);
    }

    Ok(())
}

fn spawn_dispatch<D>(
    task: ActivityTask,
    dispatcher: Arc<D>,
    result_sender: mpsc::UnboundedSender<DispatchFinished>,
    permit: OwnedSemaphorePermit,
) where
    D: ActivityDispatcher,
{
    info!(
        activity_type = %task.activity_type,
        activity_id = task.activity_id.sequence_position(),
        workflow_id = %task.workflow_id,
        attempt = task.attempt,
        "received activity task"
    );
    let workflow_id = task.workflow_id.clone();
    let activity_id = task.activity_id.clone();
    tokio::spawn(async move {
        let outcome = dispatcher.dispatch(task).await;
        let finished = DispatchFinished {
            workflow_id,
            activity_id,
            outcome,
        };
        if result_sender.send(finished).is_err() {
            debug!("worker loop stopped before dispatch outcome could be delivered");
        }
        drop(permit);
    });
}

async fn drain_finished_reports<S>(
    session: &mut S,
    tracker: &mut UnackedResultTracker,
    result_receiver: &mut mpsc::UnboundedReceiver<DispatchFinished>,
    in_flight: &mut usize,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    while let Ok(finished) = result_receiver.try_recv() {
        report_finished(session, tracker, finished, pending_error).await;
        *in_flight = in_flight.saturating_sub(1);
    }
}

async fn report_finished<S>(
    session: &mut S,
    tracker: &mut UnackedResultTracker,
    finished: DispatchFinished,
    pending_error: &mut Option<WorkerError>,
) where
    S: WorkerSession,
{
    match finished.outcome {
        Ok(outcome) => record_first_error(
            pending_error,
            report_outcome(
                session,
                tracker,
                finished.workflow_id,
                finished.activity_id,
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
    tracker: &mut UnackedResultTracker,
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
            tracker.record(PendingActivityReport::Completed {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
                output: output.clone(),
            });
            session
                .report_result(workflow_id, activity_id.clone(), output)
                .await?;
            info!(
                activity_id = activity_id.sequence_position(),
                "reported activity result"
            );
        }
        DispatchOutcome::Failed { failure } => {
            tracker.record(PendingActivityReport::Failed {
                workflow_id: workflow_id.clone(),
                activity_id: activity_id.clone(),
                failure: failure.clone(),
            });
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
    workflow_id: WorkflowId,
    activity_id: ActivityId,
    outcome: Result<DispatchOutcome, WorkerError>,
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
