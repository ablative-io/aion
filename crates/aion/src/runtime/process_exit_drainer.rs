//! Channel-parking loop for the runtime's singleton process-exit drainer.

use std::sync::Weak;
use std::sync::atomic::Ordering;

use beamr::scheduler::{ExitEventRecvError, ExitEventSubscription, Scheduler};

use crate::EngineError;

use super::{ProcessExitObservationFailure, ProcessExitRegistry};

pub(super) fn run(
    registry: &Weak<ProcessExitRegistry>,
    scheduler: &Scheduler,
    subscription: &ExitEventSubscription,
) -> Result<(), EngineError> {
    let mut shutdown_resynchronized = false;
    loop {
        let Some(registry) = registry.upgrade() else {
            return Ok(());
        };
        if registry.stop_drainer.load(Ordering::Acquire) {
            if !shutdown_resynchronized {
                registry.resynchronize(scheduler)?;
                shutdown_resynchronized = true;
            }
            if registry.all_records_terminal()? {
                return Ok(());
            }
        }
        match subscription.recv_timeout(registry.park_bound) {
            Ok(event) => {
                #[cfg(test)]
                registry.pause_if_requested();
                if let Err(error) = registry.process_event(scheduler, event) {
                    tracing::error!(%error, "process exit drainer invariant failed");
                    registry
                        .fail_unobserved(ProcessExitObservationFailure::OutcomeMissingAfterEvent);
                    return Err(error);
                }
            }
            Err(ExitEventRecvError::Timeout) => {}
            Err(ExitEventRecvError::Disconnected) => {
                registry.fail_unobserved(ProcessExitObservationFailure::EventStreamDisconnected);
                return Err(EngineError::ProcessExitEventStreamDisconnected);
            }
        }
    }
}
