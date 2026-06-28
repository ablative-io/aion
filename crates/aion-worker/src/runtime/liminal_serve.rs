//! Reconnect-to-survivor serve entry point for the liminal worker (G-1, #112).
//!
//! [`serve_with_redial`] wires the real liminal [`LiminalActivityWorker`] connect
//! + serve into the transport-free [`run_redial_loop`](crate::runtime::liminal_redial::run_redial_loop):
//! it dials a STATIC list of candidate listen addresses and, whenever the current
//! connection drops, migrates to the next candidate — re-registering in whichever
//! server's per-process registry it lands on. That migration is what lets an
//! in-flight fan-out complete after the owner of its shard is `kill -9`'d.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::activity::ActivityRegistry;
use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::runtime::liminal::LiminalActivityWorker;
use crate::runtime::liminal_redial::{
    CandidateCursor, RedialBackoff, RedialError, ServeResult, run_redial_loop,
};

/// Serves activities across a STATIC list of candidate liminal listen addresses,
/// migrating to the next candidate whenever the current connection drops (G-1,
/// #112).
///
/// On startup, and after every connection drop, this dials the cursor's current
/// candidate via [`LiminalActivityWorker::connect`] — which re-runs the in-band
/// `WorkerRegister`/`Ack`, so a redialed worker RE-REGISTERS in whichever
/// server's per-process registry it lands on. When the owner of a shard is
/// `kill -9`'d, the in-flight connection drops, the driver advances to the
/// survivor candidate, and the re-registration makes the worker selectable on
/// the survivor that adopted the shard — closing the liminal-push failover gap.
///
/// `stop` is a shared flag (the worker sets it from a signal handler or another
/// thread): it is checked between candidates AND inside each connection's serve
/// loop, so a shutdown is honoured promptly even on a quiet connection. Reconnect
/// attempts use bounded exponential backoff (`initial_backoff`..`max_backoff`)
/// that resets after a connection that served work, so a survivor whose listener
/// is briefly not up is retried without hot-spinning.
///
/// `on_first_ready` is invoked exactly once, right after the FIRST successful
/// registration, so a caller can publish a readiness observable (e.g. a file the
/// failover test polls) without racing a sleep.
///
/// # Errors
///
/// Returns [`WorkerError`] only on a NON-retryable connect failure (a
/// deterministic registration rejection — see [`WorkerError::is_retryable`]); a
/// missing candidate list surfaces as a [`WorkerError::Registration`] wrapping
/// [`RedialError`]. Retryable transport failures are absorbed into the redial
/// cycle and never surfaced.
pub fn serve_with_redial<Ready>(
    candidates: Vec<String>,
    config: &WorkerConfig,
    registry: &Arc<ActivityRegistry>,
    initial_backoff: Duration,
    max_backoff: Duration,
    stop: &AtomicBool,
    mut on_first_ready: Ready,
) -> Result<(), WorkerError>
where
    Ready: FnMut() + Send,
{
    let mut cursor = CandidateCursor::new(candidates).map_err(redial_setup_error)?;
    let mut backoff = RedialBackoff::new(initial_backoff, max_backoff);

    // The push receive is blocking, so each connection's async serve loop runs on
    // a dedicated current-thread runtime created HERE (not nested inside another
    // runtime), matching the worker binary's existing single-runtime shape.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(WorkerError::registration)?;

    let mut announced_ready = false;
    let connect = |address: &str| -> Result<LiminalActivityWorker, WorkerError> {
        LiminalActivityWorker::connect(address, config, Arc::clone(registry))
    };
    let serve = |worker: LiminalActivityWorker| -> ServeResult {
        if !announced_ready {
            announced_ready = true;
            on_first_ready();
        }
        runtime.block_on(worker.serve_until_drop(|| stop.load(Ordering::Relaxed)))
    };

    run_redial_loop(
        &mut cursor,
        &mut backoff,
        connect,
        serve,
        std::thread::sleep,
        || stop.load(Ordering::Relaxed),
        WorkerError::is_retryable,
    )
}

/// Wraps a [`RedialError`] (an empty candidate list) as a worker registration
/// error so the redial entry point has a single error type.
fn redial_setup_error(error: RedialError) -> WorkerError {
    WorkerError::registration(error)
}
