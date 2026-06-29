//! End-to-end smoke test for the unified binary's `server` subcommand: the
//! `aion` executable boots the full workflow server from a `--config` file,
//! serves an HTTP request, and drains cleanly on SIGTERM with exit code 0.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

type TestError = Box<dyn std::error::Error>;

const BOOT_DEADLINE: Duration = Duration::from_secs(60);
const EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// Reserve a loopback port by binding to port 0 and dropping the listener.
///
/// A small race window exists between drop and the server's own bind; the
/// kernel does not reissue recently-bound ephemeral ports eagerly, so this
/// is the standard fixture approach used across the workspace.
fn reserve_port() -> Result<u16, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn write_config(directory: &Path, http_port: u16, grpc_port: u16) -> Result<String, TestError> {
    let config = format!(
        r#"workflow_packages = []

[server]
listen_address = "127.0.0.1:{http_port}"
grpc_address = "127.0.0.1:{grpc_port}"

[store]
backend = "memory"

[runtime]
query_timeout_ms = 10000

[namespaces]
default = "default"

[websocket]
event_broadcast_capacity = 1024
cluster_broadcast_capacity = 64
"#
    );
    let path = directory.join("server-config.toml");
    std::fs::write(&path, config)?;
    Ok(path.to_string_lossy().into_owned())
}

/// Issue a raw `GET /health/live` and return the full HTTP response.
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

fn wait_for_exit(child: &mut Child, deadline: Duration) -> Result<Option<i32>, TestError> {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill()?;
    Err("server did not exit within the drain deadline".into())
}

#[test]
fn server_subcommand_boots_serves_and_drains_cleanly() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let http_port = reserve_port()?;
    let grpc_port = reserve_port()?;
    let config_path = write_config(temp_dir.path(), http_port, grpc_port)?;

    // Run from the temp directory so package auto-discovery cannot pick up
    // stray `.aion` archives from the workspace.
    let mut child = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(["server", "--config", &config_path])
        .current_dir(temp_dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Boot: poll the health probe until the HTTP transport answers.
    let started = Instant::now();
    let response = loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "server exited during boot with {status}; output:\n{}",
                captured_output(&mut child)
            )
            .into());
        }
        if let Some(response) = http_get_live(http_port) {
            break response;
        }
        if started.elapsed() > BOOT_DEADLINE {
            child.kill()?;
            return Err(format!(
                "server did not answer /health/live within {BOOT_DEADLINE:?}; output:\n{}",
                captured_output(&mut child)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "liveness probe must answer 200: {response}"
    );

    // Shutdown: first SIGTERM drains gracefully and exits clean (code 0).
    let term = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()?;
    assert!(term.success(), "failed to deliver SIGTERM");
    let exit_code = wait_for_exit(&mut child, EXIT_DEADLINE)?;
    let output = captured_output(&mut child);
    assert_eq!(
        exit_code,
        Some(0),
        "graceful drain must exit 0; output:\n{output}"
    );
    assert!(
        output.contains("aion-server startup banner"),
        "startup banner must be logged; output:\n{output}"
    );
    assert!(
        output.contains("beginning graceful drain"),
        "drain start must be logged; output:\n{output}"
    );
    Ok(())
}
