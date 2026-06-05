//! receive->dispatch->report worker loop + bounded concurrency

use std::collections::BTreeSet;
use std::sync::Arc;

use aion_core::{ActivityError, ActivityId, Payload, WorkflowId};
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info};

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::protocol::{ActivityTask, WorkerSession};

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Activity completed with an output payload.
    Completed {
        /// Owning workflow id.
        workflow_id: WorkflowId,
        /// Completed activity id.
        activity_id: ActivityId,
        /// Opaque output payload.
        output: Payload,
    },
    /// Activity failed with explicit classification.
    Failed {
        /// Owning workflow id.
        workflow_id: WorkflowId,
        /// Failed activity id.
        activity_id: ActivityId,
        /// Classified activity failure.
        failure: ActivityError,
    },
}

impl DispatchOutcome {
    fn activity_id(&self) -> &ActivityId {
        match self {
            Self::Completed { activity_id, .. } | Self::Failed { activity_id, .. } => activity_id,
        }
    }
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
            &mut result_receiver,
            &mut in_flight,
            &mut pending_error,
        )
        .await?;
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
            &mut result_receiver,
            &mut in_flight,
            &mut pending_error,
        )
        .await?;
        if pending_error.is_some() {
            drop(permit);
            break;
        }

        match stream.next().await {
            Some(Ok(proto_task)) => {
                let task = ActivityTask::try_from(proto_task)?;
                info!(
                    activity_type = %task.activity_type,
                    activity_id = task.activity_id.sequence_position(),
                    workflow_id = %task.workflow_id,
                    attempt = task.attempt,
                    "received activity task"
                );
                let task_dispatcher = Arc::clone(&dispatcher);
                let task_result_sender = result_sender.clone();
                in_flight += 1;
                tokio::spawn(async move {
                    let outcome = task_dispatcher.dispatch(task).await;
                    let finished = DispatchFinished { outcome };
                    if task_result_sender.send(finished).is_err() {
                        debug!("worker loop stopped before dispatch result could be delivered");
                    }
                    drop(permit);
                });
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
                in_flight -= 1;
                report_finished(session, finished, &mut pending_error).await?;
            }
            None => break,
        }
    }

    if let Some(error) = pending_error {
        return Err(error);
    }

    Ok(())
}

async fn drain_finished_reports<S>(
    session: &mut S,
    result_receiver: &mut mpsc::UnboundedReceiver<DispatchFinished>,
    in_flight: &mut usize,
    pending_error: &mut Option<WorkerError>,
) -> Result<(), WorkerError>
where
    S: WorkerSession,
{
    while let Ok(finished) = result_receiver.try_recv() {
        *in_flight = in_flight.saturating_sub(1);
        report_finished(session, finished, pending_error).await?;
    }
    Ok(())
}

async fn report_finished<S>(
    session: &mut S,
    finished: DispatchFinished,
    pending_error: &mut Option<WorkerError>,
) -> Result<(), WorkerError>
where
    S: WorkerSession,
{
    match finished.outcome {
        Ok(outcome) => report_outcome(session, outcome).await,
        Err(error) => {
            if pending_error.is_none() {
                *pending_error = Some(error);
            }
            Ok(())
        }
    }
}

async fn report_outcome<S>(session: &mut S, outcome: DispatchOutcome) -> Result<(), WorkerError>
where
    S: WorkerSession,
{
    debug!(
        activity_id = outcome.activity_id().sequence_position(),
        "reporting activity outcome"
    );
    match outcome {
        DispatchOutcome::Completed {
            workflow_id,
            activity_id,
            output,
        } => {
            session
                .report_result(workflow_id, activity_id.clone(), output)
                .await?;
            info!(
                activity_id = activity_id.sequence_position(),
                "reported activity result"
            );
        }
        DispatchOutcome::Failed {
            workflow_id,
            activity_id,
            failure,
        } => {
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
    outcome: Result<DispatchOutcome, WorkerError>,
}

#[derive(Debug, thiserror::Error)]
#[error("worker max_concurrency must be greater than zero")]
struct InvalidMaxConcurrency;

#[cfg(test)]
#[path = "loop_tests.rs"]
mod tests;
