//! Minimal liminal PUSH fan-out worker for the LSUB-5-B OS-process spike, with
//! reconnect-to-survivor (G-1, #112).
//!
//! Connects IN to a deployed `aion server`'s `outbox.liminal_listen_address`
//! over the liminal server-push transport, self-registers in-band for the pool
//! `(default, default)`, and serves the `collect_four` fixture's `fan:N`
//! activities (returning a deterministic per-ordinal result), until killed.
//!
//! Usage:
//!   liminal-fan-worker --address 127.0.0.1:PORT [--address 127.0.0.1:PORT2 ...]
//!                      [--identity <id>] [--ready-file <path>]
//!
//! Pass `--address` ONCE PER candidate liminal listener. The worker dials the
//! first candidate and, whenever that connection drops (e.g. its `aion server`
//! is `kill -9`'d), it MIGRATES to the next candidate — re-running the in-band
//! `WorkerRegister`/`Ack` so it re-registers in the SURVIVOR's per-process
//! connected-worker registry and becomes selectable there. This is the
//! liminal-push counterpart of the gRPC worker's per-endpoint redial; it is what
//! lets an in-flight fan-out complete after the owner of its shard dies.
//!
//! This is the OS-process counterpart of the in-process `LiminalActivityWorker`
//! used by `crates/aion-server/tests/lsub5_xnode_failover_e2e.rs` and
//! `run.rs::lsub_prod_xnode_e2e`, driving the production push path verbatim via
//! `aion_worker::serve_with_redial`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aion_worker::{ActivityContext, ActivityRegistry, HandlerFuture, WorkerConfig};

/// The fan-out arity of the `collect_four` fixture.
const FAN_OUT: usize = 4;

/// The activity types `collect_four` dispatches, one per fan-out ordinal.
const FAN_ACTIVITY_TYPES: [&str; FAN_OUT] = ["fan:0", "fan:1", "fan:2", "fan:3"];

/// Lower bound on the reconnect backoff between candidate dials.
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
/// Upper bound on the reconnect backoff (a survivor may take a moment to adopt
/// the shard and bring its listener up).
const REDIAL_MAX_BACKOFF: Duration = Duration::from_millis(500);

/// The fixture passes each member the JSON string `"in"`, so the handler decodes
/// a [`String`] (matching `run.rs::lsub_prod_xnode_e2e::FanInput`).
type FanInput = String;

/// Per-activity artificial delay, in milliseconds, read from
/// `LIMINAL_FAN_DELAY_MS` (default 0). The kill-9 failover gate sets this so the
/// fan-out cannot finish before the owner is killed, forcing the survivor to
/// re-dispatch the still-pending ordinals to the redialed worker.
fn fan_delay() -> std::time::Duration {
    let millis = std::env::var("LIMINAL_FAN_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    std::time::Duration::from_millis(millis)
}

/// Build the activity registry: one handler per `fan:N` type, each returning the
/// activity-type name as its deterministic result string after the configured
/// per-activity delay.
fn build_registry() -> anyhow::Result<Arc<ActivityRegistry>> {
    let mut registry = ActivityRegistry::new();
    let delay = fan_delay();
    for activity_type in FAN_ACTIVITY_TYPES {
        registry = registry.register_activity(
            activity_type,
            move |_input: FanInput, _context: &ActivityContext| -> HandlerFuture<'_, String> {
                Box::pin(async move {
                    tracing::info!(activity = %activity_type, "serving liminal fan-out dispatch");
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    Ok(activity_type.to_owned())
                })
            },
        )?;
    }
    Ok(Arc::new(registry))
}

/// Parsed command-line arguments.
struct Args {
    /// One or more candidate liminal listen addresses, in dial-preference order.
    candidates: Vec<String>,
    /// The worker identity announced in-band.
    identity: String,
    /// Optional readiness file written once after the first registration.
    ready_file: Option<String>,
}

/// Parse `--address` (repeatable), `--identity`, and `--ready-file`.
fn parse_args() -> anyhow::Result<Args> {
    let mut candidates: Vec<String> = Vec::new();
    let mut identity = "liminal-fan-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => match args.next() {
                Some(value) => candidates.push(value),
                None => anyhow::bail!("--address requires a value"),
            },
            "--identity" => {
                if let Some(value) = args.next() {
                    identity = value;
                }
            }
            "--ready-file" => {
                ready_file = args.next();
            }
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        // Preserve the single-address default the earlier spike used so existing
        // invocations without --address still start (against the default port).
        candidates.push("127.0.0.1:50061".to_owned());
    }
    Ok(Args {
        candidates,
        identity,
        ready_file,
    })
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    tracing::info!(candidates = ?args.candidates, identity = %args.identity, "liminal-fan-worker starting");

    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue("default")
        .identity(&args.identity)
        .max_concurrency(FAN_OUT)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(3)
        .build()?;

    let registry = build_registry()?;

    // Never stop on our own; the OS-process test ends the worker with a kill.
    let stop = AtomicBool::new(false);
    let ready_file = args.ready_file.clone();
    // `serve_with_redial` dials the first candidate, serves until the connection
    // drops, then migrates to the next candidate (re-registering there) with
    // bounded backoff — the reconnect-to-survivor capability. Readiness is
    // published on the first registration via a real observable (the file the
    // test polls), never a sleep.
    aion_worker::serve_with_redial(
        args.candidates,
        &config,
        &registry,
        REDIAL_INITIAL_BACKOFF,
        REDIAL_MAX_BACKOFF,
        &stop,
        move || {
            if let Some(path) = ready_file.as_ref()
                && let Err(error) = std::fs::write(path, b"connected")
            {
                tracing::error!(%error, "failed to write worker readiness file");
            }
            tracing::info!("liminal-fan-worker connected and registered; serving pushes");
        },
    )?;

    Ok(())
}
