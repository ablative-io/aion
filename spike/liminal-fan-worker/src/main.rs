//! Minimal liminal PUSH fan-out worker for the LSUB-5-B OS-process spike.
//!
//! Connects IN to a deployed `aion server`'s `outbox.liminal_listen_address`
//! over the liminal server-push transport, self-registers in-band for the pool
//! `(default, default)`, and serves the `collect_four` fixture's `fan:N`
//! activities (returning a deterministic per-ordinal result), until killed.
//!
//! Usage:
//!   liminal-fan-worker --address 127.0.0.1:PORT [--identity <id>]
//!                      [--ready-file <path>]
//!
//! This is the OS-process counterpart of the in-process `LiminalActivityWorker`
//! used by `crates/aion-server/tests/lsub5_xnode_failover_e2e.rs` and
//! `run.rs::lsub_prod_xnode_e2e`. It deliberately uses the SAME
//! `LiminalActivityWorker::connect` + `serve_until` shape so the binary exercises
//! the production push path verbatim.
//!
//! KNOWN SEAM WALL (LSUB-5-B): `LiminalActivityWorker` connects to ONE address
//! and `serve_until` returns the FIRST transport error (it has no
//! reconnect-to-survivor logic; the underlying `liminal_sdk::PushClient` is a
//! single-shot socket with a background reader, not the SDK's reconnecting
//! subscription handle). When the connected server is `kill -9`'d, this worker's
//! serve loop ends with a transport error and the process exits — it cannot
//! migrate to the survivor's distinct `liminal_listen_address`. Closing that gap
//! is a real worker-reconnect feature, not a spike. This binary therefore proves
//! the cross-process push CONNECT + DISPATCH half, and surfaces the reconnect
//! wall honestly (it exits when its server dies).

use std::sync::Arc;

use aion_worker::{ActivityContext, ActivityRegistry, HandlerFuture, WorkerConfig};

/// The fan-out arity of the `collect_four` fixture.
const FAN_OUT: usize = 4;

/// The activity types `collect_four` dispatches, one per fan-out ordinal.
const FAN_ACTIVITY_TYPES: [&str; FAN_OUT] = ["fan:0", "fan:1", "fan:2", "fan:3"];

/// The fixture passes each member the JSON string `"in"`, so the handler decodes
/// a [`String`] (matching `run.rs::lsub_prod_xnode_e2e::FanInput`).
type FanInput = String;

/// Build the activity registry: one handler per `fan:N` type, each returning the
/// activity-type name as its deterministic result string.
fn build_registry() -> anyhow::Result<Arc<ActivityRegistry>> {
    let mut registry = ActivityRegistry::new();
    for activity_type in FAN_ACTIVITY_TYPES {
        registry = registry.register_activity(
            activity_type,
            move |_input: FanInput, _context: &ActivityContext| -> HandlerFuture<'_, String> {
                Box::pin(async move {
                    tracing::info!(activity = %activity_type, "serving liminal fan-out dispatch");
                    Ok(activity_type.to_owned())
                })
            },
        )?;
    }
    Ok(Arc::new(registry))
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut address = "127.0.0.1:50061".to_owned();
    let mut identity = "liminal-fan-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => {
                if let Some(value) = args.next() {
                    address = value;
                }
            }
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

    tracing::info!(%address, %identity, "liminal-fan-worker starting");

    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue("default")
        .identity(&identity)
        .max_concurrency(FAN_OUT)
        .reconnect_initial_backoff(std::time::Duration::from_millis(5))
        .reconnect_max_backoff(std::time::Duration::from_millis(20))
        .reconnect_max_attempts(3)
        .build()?;

    let registry = build_registry()?;

    // The push receive is blocking, so drive it on a current-thread runtime —
    // exactly as the in-process `WorkerThread`/`SurvivorWorker` harnesses do.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let worker = aion_worker::LiminalActivityWorker::connect(&address, &config, registry)?;
        // The in-band WorkerRegister/Ack round-trip has completed: the server has
        // accepted this worker into its registry, so it is genuinely connected and
        // selectable. Signal readiness on a real observable (a file the test polls)
        // rather than a sleep.
        if let Some(path) = ready_file.as_ref() {
            std::fs::write(path, b"connected")?;
        }
        tracing::info!("liminal-fan-worker connected and registered; serving pushes");
        // Serve forever (until the connection drops or the process is killed).
        // `serve_until(|| false)` returns the first transport error — including
        // the connected server dying — which is the reconnect seam wall above.
        worker.serve_until(|| false).await?;
        anyhow::Ok(())
    })?;

    Ok(())
}
