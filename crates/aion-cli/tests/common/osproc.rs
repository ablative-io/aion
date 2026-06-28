//! Shared OS-process harness for the LSUB-5-B liminal failover gates.
//!
//! These helpers boot real `aion server` OS processes and a real
//! `spike/liminal-fan-worker` OS process, observe readiness on REAL observables
//! (an HTTP health probe, a worker readiness file, server log lines), and reap
//! every spawned process so a test never leaks. They are shared by the
//! connect-half test and the kill-9 reconnect-to-survivor failover test so the
//! two gates exercise the SAME boot/observe/teardown path.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    PackageBuilder,
};
use serde_json::json;

/// Error type used across the OS-process harness and its tests.
pub type TestError = Box<dyn std::error::Error>;

/// Deadline for a spawned server to answer its liveness probe.
const BOOT_DEADLINE: Duration = Duration::from_secs(90);
/// Deadline for a reaped child to exit.
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

/// The `collect_four` fixture module (same beam the in-process liminal tests load).
const OUTBOX_MODULE: &str = "aion_outbox_fixture";
const OUTBOX_BEAM: &[u8] =
    include_bytes!("../../../aion-server/tests/fixtures/aion_outbox_fixture.beam");
const OUTBOX_SOURCE: &[u8] =
    include_bytes!("../../../aion-server/tests/fixtures/aion_outbox_fixture.erl");

/// Reserve a loopback port by binding to 0 and dropping the listener.
pub fn reserve_port() -> Result<u16, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Write the `collect_four` package to disk exactly as an operator-supplied
/// `workflow_packages` archive, so the spawned server loads it through its real
/// boot path.
pub fn write_package_archive(dir: &Path) -> Result<PathBuf, TestError> {
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

/// Block until `http_get_live` answers 200 or the boot deadline elapses. Surfaces
/// the child's output if it exits during boot.
pub fn wait_for_liveness(child: &mut Child, http_port: u16) -> Result<(), TestError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "server exited during boot with {status}; output:\n{}",
                captured_output(child)
            )
            .into());
        }
        if let Some(response) = http_get_live(http_port) {
            if response.starts_with("HTTP/1.1 200") {
                return Ok(());
            }
            return Err(format!("liveness probe must answer 200: {response}").into());
        }
        if started.elapsed() > BOOT_DEADLINE {
            return Err(format!(
                "server did not answer /health/live within {BOOT_DEADLINE:?}; output:\n{}",
                captured_output(child)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
pub fn reap(mut child: Child) {
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
pub fn build_worker_binary() -> Result<PathBuf, TestError> {
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
