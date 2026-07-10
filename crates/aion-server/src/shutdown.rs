//! Graceful shutdown and single-node activity drain coordination.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::ServerState;
use crate::error::ServerError;
use crate::worker::LostWorkerReport;

/// Process exit selected by the shutdown coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownOutcome {
    /// Drain completed before the configured timeout.
    Clean,
    /// In-flight activities outlived the drain timeout and were parked for
    /// restart recovery (#207): nothing recorded, nothing delivered — the
    /// recoverable-by-design state, so a fully-parked drain is a SUCCESS. A
    /// long-running activity (an agent round runs hours) outliving any sane
    /// drain window is the expected case, and a non-zero exit on every routine
    /// deploy would train operators to ignore failures.
    Parked,
    /// The drain timed out AND the park itself failed (lock poison, sink
    /// error): in-flight state could not be handed to restart recovery.
    TimedOut,
    /// A second termination signal requested immediate process exit.
    Forced,
}

impl ShutdownOutcome {
    /// Convert the outcome to the process exit code required by the operations contract.
    #[must_use]
    pub fn exit_code(self) -> ExitCode {
        match self {
            Self::Clean | Self::Parked => ExitCode::SUCCESS,
            Self::TimedOut => ExitCode::FAILURE,
            Self::Forced => ExitCode::from(130),
        }
    }
}

/// Cloneable gate shared by transports, dispatchers, worker streams, and the
/// shutdown coordinator.
#[derive(Clone, Debug, Default)]
pub struct DrainState {
    inner: Arc<DrainStateInner>,
}

#[derive(Debug, Default)]
struct DrainStateInner {
    draining: AtomicBool,
    empty: Notify,
}

impl DrainState {
    /// Return whether drain has begun and new workflow/activity starts must be rejected.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }

    /// Mark the server draining. Returns true for the first caller that changed the state.
    #[must_use]
    pub fn begin(&self) -> bool {
        !self.inner.draining.swap(true, Ordering::AcqRel)
    }

    /// Reject a new unit of work if drain has already begun.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::WorkerDispatch`] with a stable drain message when work is closed.
    pub fn ensure_accepting(
        &self,
        namespace: &str,
        activity_type: &str,
    ) -> Result<(), ServerError> {
        if self.is_draining() {
            Err(ServerError::worker_dispatch(
                namespace.to_owned(),
                activity_type.to_owned(),
                "server is draining and not accepting new activity tasks",
            ))
        } else {
            Ok(())
        }
    }

    /// Wake waiters after in-flight accounting may have reached zero.
    pub fn notify_activity_drained(&self) {
        self.inner.empty.notify_waiters();
    }

    async fn wait_for_empty(&self, state: &ServerState) -> Result<(), ServerError> {
        loop {
            let in_flight = state.heartbeat_tracker().in_flight_count()?;
            if in_flight == 0 {
                return Ok(());
            }
            let notified = self.inner.empty.notified();
            if state.heartbeat_tracker().in_flight_count()? == 0 {
                return Ok(());
            }
            notified.await;
        }
    }
}

/// Run the graceful drain after the first termination signal.
///
/// The caller is responsible for stopping transports as soon as drain begins.
///
/// # Errors
///
/// Returns [`ServerError`] if worker-drain broadcast, in-flight accounting, timeout failure
/// surfacing, or engine shutdown fails.
pub async fn drain_after_first_signal(
    state: ServerState,
    second_signal: impl std::future::Future<Output = ()>,
) -> Result<ShutdownOutcome, ServerError> {
    let drain = state.drain_state().clone();
    let first = drain.begin();
    if first {
        info!("shutdown signal received; beginning graceful drain");
    }

    let delivered_workers = state.worker_registry().broadcast_drain()?;
    info!(delivered_workers, "sent drain request to connected workers");

    let timeout = state.runtime_config().drain_timeout;
    tokio::pin!(second_signal);

    let outcome = tokio::select! {
        () = &mut second_signal => {
            warn!("second shutdown signal received; forcing immediate exit");
            ShutdownOutcome::Forced
        }
        result = wait_for_drain_or_timeout(&state, &drain, timeout) => result?,
    };

    if matches!(outcome, ShutdownOutcome::Forced) {
        return Ok(outcome);
    }

    state.shutdown()?;
    Ok(outcome)
}

async fn wait_for_drain_or_timeout(
    state: &ServerState,
    drain: &DrainState,
    timeout: Duration,
) -> Result<ShutdownOutcome, ServerError> {
    match tokio::time::timeout(timeout, drain.wait_for_empty(state)).await {
        Ok(result) => {
            result?;
            info!("activity drain completed cleanly");
            Ok(ShutdownOutcome::Clean)
        }
        Err(_elapsed) => {
            // #207 drain-timeout backstop: PARK the remaining in-flight
            // dispatches for restart recovery instead of synthesizing
            // retryable lost-worker failures. Nothing is recorded, so the
            // durable log converges on the kill -9 shape and post-restart
            // replay re-dispatches every parked ordinal. A park that itself
            // fails leaves in-flight state unhanded — the one remaining
            // FAILURE-worthy drain outcome.
            match state
                .heartbeat_tracker()
                .park_all_in_flight_workers(state.worker_registry(), state.pending_activities())
            {
                Ok(reports) => {
                    log_parked_workers(&reports);
                    Ok(ShutdownOutcome::Parked)
                }
                Err(park_error) => {
                    error!(
                        %park_error,
                        "activity drain timed out and parking the remaining in-flight \
                         activities failed; exiting with the failure drain outcome"
                    );
                    Ok(ShutdownOutcome::TimedOut)
                }
            }
        }
    }
}

fn log_parked_workers(reports: &[LostWorkerReport]) {
    let parked_tasks: usize = reports.iter().map(|report| report.tasks.len()).sum();
    if parked_tasks == 0 {
        info!("activity drain timed out with no tracked in-flight activities to park");
    } else {
        info!(
            parked_workers = reports.len(),
            parked_tasks,
            "activity drain timed out; remaining activities parked for restart recovery"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use super::{DrainState, ShutdownOutcome};

    #[test]
    fn begin_is_idempotent_and_sets_draining() {
        let drain = DrainState::default();

        assert!(!drain.is_draining());
        assert!(drain.begin());
        assert!(drain.is_draining());
        assert!(!drain.begin());
    }

    /// #207 exit contract: a fully-parked drain is a SUCCESS (parked state is
    /// recoverable by design); FAILURE is reserved for a park that itself
    /// failed; a forced exit keeps 130. `ExitCode` carries no `PartialEq`, so
    /// the mapping is asserted through its debug representation.
    #[test]
    fn exit_codes_map_parked_to_success_and_timed_out_to_failure() {
        let debug = |code: ExitCode| format!("{code:?}");
        assert_eq!(
            debug(ShutdownOutcome::Clean.exit_code()),
            debug(ExitCode::SUCCESS)
        );
        assert_eq!(
            debug(ShutdownOutcome::Parked.exit_code()),
            debug(ExitCode::SUCCESS)
        );
        assert_eq!(
            debug(ShutdownOutcome::TimedOut.exit_code()),
            debug(ExitCode::FAILURE)
        );
        assert_eq!(
            debug(ShutdownOutcome::Forced.exit_code()),
            debug(ExitCode::from(130))
        );
    }
}
