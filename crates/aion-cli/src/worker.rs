//! The `aion worker serve` subcommand: the production worker-serve entrypoint
//! (NOI-5b/NOI-6).
//!
//! This is the shipped-binary counterpart of the `spike/liminal-fan-worker`
//! process: it connects IN to one or more deployed `aion server`
//! `outbox.liminal_listen_address` listeners over the liminal server-push
//! transport, self-registers in-band, and serves pushed dispatches — driving the
//! reconnect-to-survivor redial loop ([`aion_worker::serve_with_redial`]) so an
//! in-flight fan-out completes after the owner of its shard dies.
//!
//! It is THE production path that installs the composed agent harness: the
//! composition root ([`crate::harness::default_agent_harness_config`]) hands the
//! default Norn harness — ERASED to `Arc<dyn DynAgentHarness>` — plus the
//! operator-named agent activity types and the advertised capabilities to the
//! served worker, so agent activities actually run live in a real deployment. The
//! whole path is gated on the `norn` composition-root feature (which pulls in
//! `aion-worker/liminal-transport`); a `--no-default-features` build without it
//! carries no worker-serve subcommand and names no harness type.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aion_worker::{ActivityRegistry, RedialTiming, WorkerConfig};
use anyhow::{Context, Result};
use clap::Args;

use crate::harness::default_agent_harness_config;

/// Lower bound on the reconnect backoff between candidate dials.
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
/// Upper bound on the reconnect backoff (a survivor may take a moment to adopt the
/// dead owner's shard and bring its listener up).
const REDIAL_MAX_BACKOFF: Duration = Duration::from_millis(500);
/// Reconnect attempt ceiling reported in the worker config (the redial loop
/// migrates across candidates rather than exhausting this per-endpoint).
const REDIAL_MAX_ATTEMPTS: usize = 3;

/// Arguments for `aion worker serve`.
#[derive(Args, Clone, Debug)]
pub struct WorkerServeArgs {
    /// A candidate liminal listen address (`host:port`) to dial. Repeat once per
    /// candidate, in dial-preference order: the worker dials the first and, when a
    /// connection drops, migrates to the next — re-registering there.
    #[arg(long = "address", required = true)]
    addresses: Vec<String>,
    /// An activity type this worker drives through the composed agent harness.
    /// Repeat once per agent activity type. Any activity type NOT listed here runs
    /// through the plain typed registry (empty in this serve mode).
    #[arg(long = "agent-activity", required = true)]
    agent_activities: Vec<String>,
    /// The namespace the worker registers under.
    #[arg(long, default_value = "default")]
    namespace: String,
    /// The task queue the worker registers under.
    #[arg(long = "task-queue", default_value = "default")]
    task_queue: String,
    /// The locality node the worker registers under. Empty means unpinned.
    #[arg(long, default_value = "")]
    node: String,
    /// The worker identity announced in-band.
    #[arg(long, default_value = "aion-worker")]
    identity: String,
    /// Maximum concurrent in-flight dispatches.
    #[arg(long = "max-concurrency", default_value_t = 4)]
    max_concurrency: usize,
}

/// Serves agent activities over liminal until interrupted, driving the
/// reconnect-to-survivor redial loop with the composed agent harness installed.
///
/// This blocks on the serve loop (the push receive is blocking); it returns when
/// the redial loop ends on a non-retryable connect failure. `Ctrl-C` handling is
/// left to the process signal (the loop runs until the connection can no longer be
/// re-established), mirroring the spike worker's kill-terminated lifecycle.
pub fn serve(args: &WorkerServeArgs) -> Result<()> {
    let config = build_worker_config(args).context("building the worker config")?;
    // Agent activities are driven by the harness, not the registry, so the plain
    // registry is empty in this serve mode.
    let registry = Arc::new(ActivityRegistry::new());
    // The composition root: the default Norn harness ERASED to the neutral
    // `Arc<dyn DynAgentHarness>`, bound to the operator-named agent activity types
    // and the advertised capabilities. This is the wiring the served worker was
    // missing — without it a deployed worker had no harness and agent activities
    // could not run live.
    let agent = default_agent_harness_config(args.agent_activities.clone());

    let stop = AtomicBool::new(false);
    aion_worker::serve_with_redial(
        args.addresses.clone(),
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        Some(&agent),
        || eprintln!("aion worker serve: connected and registered; serving pushes"),
    )
    .context("serving liminal worker dispatches")
}

/// Builds the worker registration config from the parsed arguments.
fn build_worker_config(args: &WorkerServeArgs) -> Result<WorkerConfig> {
    WorkerConfig::builder()
        // The liminal serve path dials the `--address` candidates directly; the
        // endpoint field is required by the builder but unused on this transport.
        .endpoint("unused-direct-address")
        .namespace(&args.namespace)
        .task_queue(&args.task_queue)
        .node(&args.node)
        .identity(&args.identity)
        .max_concurrency(args.max_concurrency)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(REDIAL_MAX_ATTEMPTS)
        .build()
        .map_err(anyhow::Error::from)
}
