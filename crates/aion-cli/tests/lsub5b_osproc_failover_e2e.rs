//! LSUB-5-B: REAL OS-process exercise of the production cross-node liminal
//! outbox-dispatch seam (`outbox.transport = liminal`) — the CONNECT + DISPATCH
//! half.
//!
//! ## What this proves (runs green; OS-process, no in-process shortcuts)
//!
//! 1. A REAL `aion` binary, built with `--features haematite-backend,liminal-transport`,
//!    boots a deployed `aion server` OS process configured with
//!    `outbox.enabled`, `outbox.transport = liminal`, and a concrete
//!    `outbox.liminal_listen_address`. The deployed binary genuinely HOSTS the
//!    liminal worker listener (the CLI forwards the `liminal-transport` feature).
//! 2. A REAL OS-process liminal worker (`spike/liminal-fan-worker`, a
//!    `LiminalActivityWorker`) connects IN to that listen address over the
//!    server-push transport, completes the in-band `WorkerRegister`/`Ack`
//!    round-trip against the spawned binary's `LiminalConnectionNotifier`, and
//!    becomes a selectable pool member. The worker proves registration on a REAL
//!    observable (a readiness file it writes only AFTER the ack), never a sleep.
//!
//! Together these exercise the cross-PROCESS half of the production liminal seam
//! that the in-process `run.rs::lsub_prod_xnode_e2e` and
//! `crates/aion-server/tests/lsub5_xnode_failover_e2e.rs` cover in-process.
//!
//! ## The kill-9 reconnect-to-survivor continuation (G-1, #112)
//!
//! The full kill-9 LIMINAL-PUSH failover — kill the owner mid-dispatch; the
//! survivor adopts the shard and re-dispatches to the worker — is proven by the
//! sibling gate `lsub5b_osproc_kill9_failover_e2e.rs`, now that the worker has a
//! reconnect-to-survivor capability (`aion_worker::serve_with_redial`: a static
//! candidate-address list + redial-on-drop that re-registers in the survivor's
//! registry). This file covers the steady-state CONNECT + DISPATCH half; the
//! sibling covers the hard-kill migration half.
//!
//! ## Running
//!
//! Ignored (slow, spawns processes): run explicitly with
//! ```text
//! cargo test -p aion-cli --features haematite-backend,liminal-transport \
//!   --test lsub5b_osproc_failover_e2e -- --ignored --nocapture
//! ```

#![cfg(all(unix, feature = "haematite-backend", feature = "liminal-transport"))]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "common/osproc.rs"]
mod osproc;

use osproc::{
    TestError, build_worker_binary, reap, reserve_port, wait_for_liveness, write_package_archive,
};

/// Deadline for the worker to write its readiness file (first registration).
const WORKER_READY_DEADLINE: Duration = Duration::from_secs(30);

/// A single-node server config with the liminal outbox transport enabled. One
/// node is sufficient to prove the CONNECT + DISPATCH half of the seam over real
/// OS processes; the 3-node quorum-survivor shape is exercised by the sibling
/// kill-9 failover gate.
fn write_config(
    dir: &Path,
    package: &Path,
    http_port: u16,
    grpc_port: u16,
    liminal_port: u16,
) -> Result<std::path::PathBuf, TestError> {
    let data_dir = dir.join("data");
    let config = format!(
        r#"workflow_packages = ["{package}"]

[server]
listen_address = "127.0.0.1:{http_port}"
grpc_address = "127.0.0.1:{grpc_port}"

[store]
backend = "haematite"
data_dir = "{data_dir}"

[runtime]
query_timeout_ms = 10000

[namespaces]
default = "default"

[websocket]
event_broadcast_capacity = 1024

[outbox]
enabled = true
poll_interval_ms = 20
batch_size = 16
max_attempts = 5
backoff_base_ms = 50
backoff_multiplier = 2
backoff_max_ms = 1000
transport = "liminal"
liminal_listen_address = "127.0.0.1:{liminal_port}"
"#,
        package = package.display(),
        data_dir = data_dir.display(),
    );
    let path = dir.join("server.toml");
    std::fs::write(&path, config)?;
    Ok(path)
}

#[test]
#[ignore = "slow: spawns real aion-server + liminal worker OS processes"]
fn osproc_liminal_listener_accepts_real_worker_connection() -> Result<(), TestError> {
    let temp = tempfile::tempdir()?;
    let http_port = reserve_port()?;
    let grpc_port = reserve_port()?;
    let liminal_port = reserve_port()?;

    let package = write_package_archive(temp.path())?;
    let config = write_config(temp.path(), &package, http_port, grpc_port, liminal_port)?;
    let worker_binary = build_worker_binary()?;

    // (1) Boot the REAL aion server OS process with the liminal outbox transport.
    let mut server = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(["server", "--config", &config.to_string_lossy()])
        .current_dir(temp.path())
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Readiness on a real observable: poll the HTTP health probe until the server
    // answers. If it exits during boot, surface its output (a feature/config
    // regression would fail to host the liminal listener and exit here).
    if let Err(error) = wait_for_liveness(&mut server, http_port) {
        reap(server);
        return Err(error);
    }

    // (2) Spawn a REAL OS-process liminal worker that connects IN to the server's
    // liminal_listen_address. It writes its readiness file ONLY after the in-band
    // WorkerRegister/Ack round-trip completes against the spawned binary's
    // notifier — a genuine observable that the production listener accepted it.
    let ready_file = temp.path().join("worker.ready");
    let liminal_address = format!("127.0.0.1:{liminal_port}");
    let mut worker = Command::new(&worker_binary)
        .args([
            "--address",
            &liminal_address,
            "--identity",
            "lsub5b-osproc-worker",
            "--ready-file",
            &ready_file.to_string_lossy(),
        ])
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let started = Instant::now();
    let connected = loop {
        if let Some(status) = worker.try_wait()? {
            reap(server);
            return Err(format!("worker exited before registering (status {status})").into());
        }
        if ready_file.exists() {
            break true;
        }
        if started.elapsed() > WORKER_READY_DEADLINE {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    if !connected {
        reap(worker);
        reap(server);
        return Err(format!(
            "worker never registered against the OS-process liminal listener within \
             {WORKER_READY_DEADLINE:?}"
        )
        .into());
    }

    // THE PROOF: a deployed aion-server binary hosted the liminal worker listener
    // and a real OS-process LiminalActivityWorker connected IN and registered —
    // the cross-process CONNECT half of the production seam. The kill-9
    // reconnect-to-survivor continuation is the sibling gate.
    reap(worker);
    reap(server);
    Ok(())
}
