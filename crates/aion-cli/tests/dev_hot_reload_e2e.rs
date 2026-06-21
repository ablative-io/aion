//! End-to-end coverage for `aion dev`: the instant authoring loop (WA-002 R1).
//!
//! This drives the REAL `aion` binary end to end: it scaffolds a workflow
//! project, runs `aion server` with the deploy surface enabled, runs `aion dev`
//! to watch the project, then EDITS the workflow source and asserts that the
//! new content-hash version is hot-loaded into the running engine with NO
//! restart and serves fresh runs — exactly the brief's R1 acceptance.
//!
//! The loop spawns the external `gleam` binary to rebuild on save and fetches
//! the `aion_flow` hex package, so the test is gated at runtime on a usable
//! `gleam` toolchain: when it is absent the test prints a skip line and passes,
//! never `#[ignore]`.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

type TestError = Box<dyn std::error::Error>;

const BOOT_DEADLINE: Duration = Duration::from_secs(60);
const RELOAD_DEADLINE: Duration = Duration::from_secs(180);

/// Whether a usable `gleam` binary is on PATH. The dev loop cannot rebuild
/// without it, so its absence gates this test at runtime.
fn gleam_available() -> bool {
    Command::new("gleam")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn reserve_port() -> Result<u16, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn write_server_config(
    directory: &Path,
    http_port: u16,
    grpc_port: u16,
) -> Result<PathBuf, TestError> {
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

[deploy]
enabled = true
max_archive_bytes = 16777216
max_inflated_bytes = 67108864
"#
    );
    let path = directory.join("server-config.toml");
    std::fs::write(&path, config)?;
    Ok(path)
}

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

fn wait_for_boot(child: &mut Child, http_port: u16) -> Result<(), TestError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("server exited during boot with {status}").into());
        }
        if let Some(response) = http_get_live(http_port) {
            if response.starts_with("HTTP/1.1 200") {
                return Ok(());
            }
        }
        if started.elapsed() > BOOT_DEADLINE {
            child.kill()?;
            return Err("server did not boot within the deadline".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Runs an `aion` subcommand to completion, returning stdout on success.
fn run_aion(args: &[&str], cwd: &Path) -> Result<String, TestError> {
    let output = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(args)
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "aion {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Lists the content hashes loaded for `workflow_type` over `aion versions`.
fn loaded_hashes(
    grpc_port: u16,
    workflow_type: &str,
    cwd: &Path,
) -> Result<Vec<String>, TestError> {
    let endpoint = format!("127.0.0.1:{grpc_port}");
    let stdout = run_aion(
        &[
            "--endpoint",
            &endpoint,
            "versions",
            "--workflow-type",
            workflow_type,
        ],
        cwd,
    )?;
    let value: serde_json::Value = serde_json::from_str(&stdout)?;
    Ok(value
        .as_array()
        .map(|versions| {
            versions
                .iter()
                .filter_map(|version| version["content_hash"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

#[test]
fn dev_hot_loads_a_new_content_hash_version_on_edit_without_restart() -> Result<(), TestError> {
    if !gleam_available() {
        // Runtime gate: the dev loop spawns `gleam`; without it there is nothing
        // to exercise. Skip loudly and pass, never `#[ignore]`.
        eprintln!(
            "skipping dev_hot_loads_a_new_content_hash_version_on_edit_without_restart: \
             gleam binary not available"
        );
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let workflow_type = "flow";
    let project = temp.path().join(workflow_type);

    // Scaffold a buildable hello-world workflow project. `new` takes a
    // snake_case name and creates `<name>/` in the working directory.
    run_aion(&["new", workflow_type], temp.path())?;

    // Boot a server with the deploy surface enabled (the dev loop hot-loads
    // over the operator deploy RPC).
    let http_port = reserve_port()?;
    let grpc_port = reserve_port()?;
    let config = write_server_config(temp.path(), http_port, grpc_port)?;
    let mut server = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args([
            "server",
            "--config",
            config.to_str().ok_or("non-utf8 config path")?,
        ])
        .current_dir(temp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_for_boot(&mut server, http_port)?;

    // Start `aion dev`: it primes the server with the current source (one
    // build + hot-load) and then watches.
    let gleam_path = which_gleam()?;
    let endpoint = format!("127.0.0.1:{grpc_port}");
    let mut dev = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args([
            "--endpoint",
            &endpoint,
            "dev",
            project.to_str().ok_or("non-utf8 project path")?,
            "--gleam-path",
            &gleam_path,
            "--debounce-ms",
            "100",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Wait for the priming hot-load to register a version.
    let first_hash = wait_for_a_loaded_version(grpc_port, workflow_type, temp.path(), &[])?;

    // Edit the workflow source so the rebuilt package has a NEW content hash.
    edit_workflow_source(&project, workflow_type)?;

    // The watcher rebuilds, repackages, and hot-loads the new content-hash
    // version with NO server restart; a fresh hash appears alongside the first.
    let already_loaded = std::slice::from_ref(&first_hash);
    let second_hash =
        wait_for_a_loaded_version(grpc_port, workflow_type, temp.path(), already_loaded)?;
    assert_ne!(
        first_hash, second_hash,
        "an edit must produce a new content-hash version"
    );

    // The server never restarted: the same process still answers liveness.
    assert!(
        server.try_wait()?.is_none(),
        "the engine must not restart across a hot-load"
    );
    let live = http_get_live(http_port).ok_or("server stopped answering after hot-load")?;
    assert!(live.starts_with("HTTP/1.1 200"));

    dev.kill()?;
    server.kill()?;
    let _ = dev.wait();
    let _ = server.wait();
    Ok(())
}

/// Resolves the `gleam` binary's absolute path for `--gleam-path`.
fn which_gleam() -> Result<String, TestError> {
    let output = Command::new("which").arg("gleam").output()?;
    if !output.status.success() {
        return Err("could not resolve the gleam binary path".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Polls `aion versions` until a loaded hash appears that is not in `exclude`.
fn wait_for_a_loaded_version(
    grpc_port: u16,
    workflow_type: &str,
    cwd: &Path,
    exclude: &[String],
) -> Result<String, TestError> {
    let started = Instant::now();
    loop {
        let hashes = loaded_hashes(grpc_port, workflow_type, cwd).unwrap_or_default();
        if let Some(hash) = hashes.into_iter().find(|hash| !exclude.contains(hash)) {
            return Ok(hash);
        }
        if started.elapsed() > RELOAD_DEADLINE {
            return Err("no new loaded version appeared within the reload deadline".into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Changes the workflow's behaviour (the greeting string) so the rebuilt
/// package compiles to DIFFERENT bytecode and therefore a new content hash — a
/// comment-only edit would not change the hash, since the hash is over the
/// compiled `.beam`.
fn edit_workflow_source(project: &Path, workflow_type: &str) -> Result<(), TestError> {
    let source = project.join("src").join(format!("{workflow_type}.gleam"));
    let contents = std::fs::read_to_string(&source)?;
    let edited = contents.replace("\"Hello, \"", "\"Hi there, \"");
    if edited == contents {
        return Err("expected to rewrite the greeting literal in the template".into());
    }
    std::fs::write(&source, edited)?;
    Ok(())
}
