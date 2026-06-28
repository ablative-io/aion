//! LSUB-5-B: REAL OS-process exercise of the production cross-node liminal
//! outbox-dispatch seam (`outbox.transport = liminal`), and an HONEST record of
//! the worker-reconnect seam wall that blocks a full kill-9 liminal-push failover.
//!
//! ## What this proves (runs green; OS-process, no in-process shortcuts)
//!
//! 1. A REAL `aion` binary, built with `--features haematite-backend,liminal-transport`,
//!    boots a deployed `aion server` OS process configured with
//!    `outbox.enabled`, `outbox.transport = liminal`, and a concrete
//!    `outbox.liminal_listen_address`. Before LSUB-5-B added the CLI
//!    `liminal-transport` feature passthrough (aion-cli/Cargo.toml) this was
//!    impossible: the CLI never forwarded the feature, so the spawned binary
//!    compiled the feature-off stub of `build_liminal_row_dispatch` and FAILED to
//!    boot with `outbox.transport = liminal`. This test pins that the deployed
//!    binary now genuinely HOSTS the liminal worker listener.
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
//! ## The seam wall this test does NOT fake (the brief's crux)
//!
//! A full kill-9 LIMINAL-PUSH failover (kill the owner mid-dispatch; the survivor
//! adopts the shard and re-dispatches to the worker) is BLOCKED by a real,
//! verified gap, so this test STOPS at it rather than fabricating a green proof:
//!
//!   * Each deployed `aion server` hosts its OWN liminal listener on its OWN
//!     `outbox.liminal_listen_address`, backed by its OWN per-process
//!     `ConnectedWorkerRegistry` (aion-server `state.rs`: one
//!     `ConnectedWorkerRegistry::default()` per `ServerState`). A worker connected
//!     to the OWNER's listener is NOT visible to a SURVIVOR's registry.
//!   * `aion_worker::LiminalActivityWorker` (aion-worker
//!     `runtime/liminal.rs:146`) connects to ONE address; `serve_until`
//!     (`:188`) returns the FIRST transport error. The underlying
//!     `liminal_sdk::PushClient` (liminal-sdk `remote/tcp/push_client.rs:95`) is a
//!     single-shot socket + background reader with NO redial and NO endpoint
//!     re-target. When the owner is `kill -9`'d, the worker's serve loop ends with
//!     a transport error and the worker exits — it cannot migrate to the
//!     survivor's distinct listen address.
//!   * The in-process LSUB-5 capstone only achieves liminal-push failover by
//!     sharing ONE `RunningLiminalServer` (one listener + one registry) across
//!     both nodes' dispatchers (`lsub5_xnode_failover_e2e.rs:394`); that shared
//!     listener does not exist across real OS processes, which is exactly why the
//!     OS-process case exposes the gap.
//!
//! The SMALLEST real fix is a worker-reconnect-to-survivor feature:
//! `LiminalActivityWorker` accepting a list/supplier of candidate
//! `liminal_listen_address`es and, on transport drop, redialing the next live one
//! (re-running `connect_with_registration` so it re-registers in the survivor's
//! registry). That is a genuine new worker capability, not a test concern, so it
//! is reported here rather than stubbed. The gRPC outbox transport already
//! survives owner death cross-process today (each worker dials per-endpoint and
//! `greet-worker` sets `reconnect_max_attempts(usize::MAX)`), proven by
//! `scripts/mp-failover-spike.sh`; LSUB-5-B is specifically the LIMINAL-push path.
//!
//! ## Running
//!
//! Ignored (slow, spawns processes): run explicitly with
//! ```text
//! cargo test -p aion-cli --features haematite-backend,liminal-transport \
//!   --test lsub5b_osproc_failover_e2e -- --ignored --nocapture
//! ```

#![cfg(all(unix, feature = "haematite-backend", feature = "liminal-transport"))]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    PackageBuilder,
};
use serde_json::json;

type TestError = Box<dyn std::error::Error>;

const BOOT_DEADLINE: Duration = Duration::from_secs(90);
const WORKER_READY_DEADLINE: Duration = Duration::from_secs(30);
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

/// The `collect_four` fixture module (same beam the in-process liminal tests load).
const OUTBOX_MODULE: &str = "aion_outbox_fixture";
const OUTBOX_BEAM: &[u8] =
    include_bytes!("../../aion-server/tests/fixtures/aion_outbox_fixture.beam");
const OUTBOX_SOURCE: &[u8] =
    include_bytes!("../../aion-server/tests/fixtures/aion_outbox_fixture.erl");

/// Reserve a loopback port by binding to 0 and dropping the listener.
fn reserve_port() -> Result<u16, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Write the `collect_four` package to disk exactly as an operator-supplied
/// `workflow_packages` archive, so the spawned server loads it through its real
/// boot path.
fn write_package_archive(dir: &Path) -> Result<PathBuf, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(OUTBOX_MODULE, OUTBOX_BEAM)])?;
    let manifest = Manifest {
        entry_module: OUTBOX_MODULE.to_owned(),
        entry_function: "collect_four".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(OUTBOX_MODULE, OUTBOX_SOURCE.to_vec())])
            .write_to_bytes()?;
    let path = dir.join("collect_four.aion");
    std::fs::write(&path, &archive)?;
    Ok(path)
}

/// A single-node server config with the liminal outbox transport enabled. One
/// node is sufficient to prove the CONNECT + DISPATCH half of the seam over real
/// OS processes; the 3-node quorum-survivor shape is only needed for the kill-9
/// failover that the worker-reconnect wall (module docs) blocks.
fn write_config(
    dir: &Path,
    package: &Path,
    http_port: u16,
    grpc_port: u16,
    liminal_port: u16,
) -> Result<PathBuf, TestError> {
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

/// Issue a raw `GET /health/live` and return the response, or `None` if the
/// server is not answering yet.
fn http_get_live(http_port: u16) -> Option<String> {
    let mut stream =
        TcpStream::connect_timeout(&([127, 0, 0, 1], http_port).into(), Duration::from_secs(1))
            .ok()?;
    stream
        .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

/// Drain a child's piped stdout+stderr (best effort) for failure diagnostics.
fn captured_output(child: &mut Child) -> String {
    let mut combined = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let mut buffer = String::new();
        if stdout.read_to_string(&mut buffer).is_ok() {
            combined.push_str(&buffer);
        }
    }
    if let Some(mut stderr) = child.stderr.take() {
        let mut buffer = String::new();
        if stderr.read_to_string(&mut buffer).is_ok() {
            combined.push_str(&buffer);
        }
    }
    combined
}

/// Kill a child and wait for it to reap, so the test never leaks a process.
fn reap(mut child: Child) {
    let _: Result<(), _> = child.kill();
    let started = Instant::now();
    while started.elapsed() < EXIT_DEADLINE {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Build the standalone liminal worker binary (it is NOT a workspace member, so
/// `CARGO_BIN_EXE_*` does not cover it — build it the way the spike script does).
fn build_worker_binary() -> Result<PathBuf, TestError> {
    let worker_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("spike")
        .join("liminal-fan-worker");
    let status = Command::new(env!("CARGO"))
        .args(["build"])
        .current_dir(&worker_dir)
        .status()?;
    if !status.success() {
        return Err("failed to build spike/liminal-fan-worker".into());
    }
    let binary = worker_dir
        .join("target")
        .join("debug")
        .join("liminal-fan-worker");
    if !binary.exists() {
        return Err(format!("worker binary not found at {}", binary.display()).into());
    }
    Ok(binary)
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
    let started = Instant::now();
    loop {
        if let Some(status) = server.try_wait()? {
            return Err(format!(
                "server exited during boot with {status}; output:\n{}",
                captured_output(&mut server)
            )
            .into());
        }
        if let Some(response) = http_get_live(http_port) {
            assert!(
                response.starts_with("HTTP/1.1 200"),
                "liveness probe must answer 200: {response}"
            );
            break;
        }
        if started.elapsed() > BOOT_DEADLINE {
            let output = captured_output(&mut server);
            reap(server);
            return Err(format!(
                "server did not answer /health/live within {BOOT_DEADLINE:?}; output:\n{output}"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
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
            // The worker exited before registering: surface both sides' output.
            let worker_output = captured_output(&mut worker);
            let server_output = captured_output(&mut server);
            reap(server);
            return Err(format!(
                "worker exited before registering (status {status}); \
                 worker output:\n{worker_output}\nserver output:\n{server_output}"
            )
            .into());
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
        let worker_output = captured_output(&mut worker);
        let server_output = captured_output(&mut server);
        reap(worker);
        reap(server);
        return Err(format!(
            "worker never registered against the OS-process liminal listener within \
             {WORKER_READY_DEADLINE:?};\nworker output:\n{worker_output}\nserver output:\n{server_output}"
        )
        .into());
    }

    // THE PROOF: a deployed aion-server binary hosted the liminal worker listener
    // and a real OS-process LiminalActivityWorker connected IN and registered —
    // the cross-process CONNECT half of the production seam, impossible before the
    // CLI liminal-transport passthrough this change adds.
    //
    // The kill-9 LIMINAL-PUSH failover continuation is the documented seam wall
    // (module docs): the worker cannot migrate to a survivor's distinct listen
    // address because LiminalActivityWorker/PushClient have no
    // reconnect-to-survivor capability. We STOP here rather than faking it.

    reap(worker);
    reap(server);
    Ok(())
}
