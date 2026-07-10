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
use crate::runtime::liminal::{AgentHarnessConfig, LiminalActivityWorker};
use crate::runtime::liminal_drain::LiveWriter;
use crate::runtime::liminal_redial::{
    CandidateCursor, RedialBackoff, RedialError, ServeResult, run_redial_loop,
};

/// The bounded exponential backoff bounds the redial driver applies between
/// candidate dials — the reconnect delay grows from `initial_backoff` to
/// `max_backoff` and resets after a connection that served work, so a survivor
/// whose listener is briefly not up is retried without hot-spinning.
///
/// Bundled into one value so [`serve_with_redial`] takes the timing as a single
/// argument rather than two loose `Duration`s.
#[derive(Clone, Copy, Debug)]
pub struct RedialTiming {
    /// Lower bound on the reconnect backoff between candidate dials.
    pub initial_backoff: Duration,
    /// Upper bound on the reconnect backoff.
    pub max_backoff: Duration,
}

impl RedialTiming {
    /// Builds a timing from the `initial`..`max` backoff bounds.
    #[must_use]
    pub const fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial_backoff: initial,
            max_backoff: max,
        }
    }
}

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
/// loop, so a shutdown is honoured promptly even on a quiet connection. `timing`
/// carries the reconnect backoff bounds (see [`RedialTiming`]).
///
/// `on_first_ready` is invoked exactly once, right after the FIRST successful
/// registration, so a caller can publish a readiness observable (e.g. a file the
/// failover test polls) without racing a sleep.
///
/// `agent` is the OPTIONAL composed agent harness the served worker drives
/// (NOI-5b/NOI-6): `Some(config)` installs it (via
/// [`LiminalActivityWorker::with_agent_config`]) on EVERY connection — including
/// after a redial to a survivor, so the migrated worker still owns its agent
/// activities — while `None` leaves the worker on the plain typed-registry path,
/// byte-identical to a harness-less build (`--no-default-features`). Erased to
/// `Arc<dyn DynAgentHarness>` in the config, so this platform crate never names a
/// concrete harness adapter.
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
    timing: RedialTiming,
    stop: &AtomicBool,
    agent: Option<&AgentHarnessConfig>,
    mut on_first_ready: Ready,
) -> Result<(), WorkerError>
where
    Ready: FnMut() + Send,
{
    let mut cursor = CandidateCursor::new(candidates).map_err(redial_setup_error)?;
    let mut backoff = RedialBackoff::new(timing.initial_backoff, timing.max_backoff);

    // The push receive is blocking, so each connection's async serve loop runs on
    // a dedicated current-thread runtime created HERE (not nested inside another
    // runtime), matching the worker binary's existing single-runtime shape.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(WorkerError::registration)?;

    let mut announced_ready = false;
    // The agent activity types the served worker must ADVERTISE in registration so
    // the server can select it for them — empty for a harness-less serve.
    let agent_types = agent
        .map(|config| config.agent_activity_types().clone())
        .unwrap_or_default();
    // One drain slot spanning every reconnection: each connect refreshes it with the
    // new connection's writer, so an observability drain spawned against an earlier
    // (now-dead) connection re-resolves the survivor instead of a dead socket (#254).
    let live_writer = LiveWriter::default();
    let connect = |address: &str| -> Result<LiminalActivityWorker, WorkerError> {
        // Install the composed harness on EVERY connection (including a redial), so
        // a worker that migrates to a survivor still drives its agent activities AND
        // re-advertises the agent types in that connection's registration; `None`
        // leaves it on the plain typed-registry path, unchanged. The shared drain
        // slot is adopted here so every connection's drains publish through it.
        LiminalActivityWorker::connect_advertising(
            address,
            config,
            Arc::clone(registry),
            &agent_types,
        )
        .map(|worker| {
            worker
                .with_agent_config(agent.cloned())
                .with_live_writer(live_writer.clone())
        })
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
