//! Full from-scaffold proof for the hello-world template: `aion new` →
//! `gleam build` → `aion package` → boot `aion server` against the
//! generated `aion.toml` → `aion deploy` → `aion start` → completion
//! asserted with `aion describe`. Every step drives the real binary.

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::TestError;

const BOOT_DEADLINE: Duration = Duration::from_secs(60);
const GRPC_DEADLINE: Duration = Duration::from_secs(30);
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);
const EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// Reserve a loopback port by binding to port 0 and dropping the listener
/// (the standard fixture approach used across the workspace).
fn reserve_port() -> Result<u16, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
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

/// Re-point the generated config at reserved loopback ports. The store URL
/// stays the generated relative `aion.db`, which lands in the project
/// directory because the server runs with the project as its working
/// directory — exactly the README workflow.
fn repoint_config_ports(project: &Path, http_port: u16, grpc_port: u16) -> Result<(), TestError> {
    let config_path = project.join("aion.toml");
    let config = std::fs::read_to_string(&config_path)?;
    if !config.contains("127.0.0.1:8080") || !config.contains("127.0.0.1:50051") {
        return Err(format!("generated aion.toml must carry the dev addresses:\n{config}").into());
    }
    let config = config
        .replace("127.0.0.1:8080", &format!("127.0.0.1:{http_port}"))
        .replace("127.0.0.1:50051", &format!("127.0.0.1:{grpc_port}"));
    std::fs::write(config_path, config)?;
    Ok(())
}

/// Boot `aion server --config aion.toml` from the project directory and wait
/// for the liveness probe.
fn boot_server(project: &Path, http_port: u16) -> Result<Child, TestError> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(["server", "--config", "aion.toml"])
        .env(
            "AION_HOME",
            std::env::temp_dir().join(format!("aion-e2e-home-{}", std::process::id())),
        )
        .current_dir(project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "server exited during boot with {status}; output:\n{}",
                captured_output(&mut child)
            )
            .into());
        }
        if http_get_live(http_port).is_some_and(|response| response.starts_with("HTTP/1.1 200")) {
            return Ok(child);
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
    }
}

/// Deploy the archive, retrying while the gRPC listener finishes binding.
fn deploy_archive(project: &Path, endpoint: &str) -> Result<(), TestError> {
    let started = Instant::now();
    loop {
        let output = common::run_cli(
            project,
            &["--endpoint", endpoint, "deploy", "hello_demo.aion"],
        )?;
        if output.status.code() == Some(0) {
            let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            // `freshly_loaded` may legitimately be false: the server runs
            // from the project directory and auto-discovers the packaged
            // archive at boot, so the explicit deploy is an idempotent
            // re-load of the same content hash.
            if body["workflow_type"] != "hello_demo" {
                return Err(format!("deploy must report the workflow type: {body}").into());
            }
            return Ok(());
        }
        if started.elapsed() > GRPC_DEADLINE {
            return Err(format!(
                "deploy did not succeed within {GRPC_DEADLINE:?}; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn scaffolded_hello_world_completes_against_the_generated_config() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let (project, _report) = common::scaffold_project(temp_dir.path(), "hello_demo", &[])?;
    common::patch_aion_flow_to_workspace(&project)?;
    common::gleam_build(&project)?;
    common::package_project(&project, "hello_demo")?;

    let http_port = reserve_port()?;
    let grpc_port = reserve_port()?;
    repoint_config_ports(&project, http_port, grpc_port)?;
    let mut server = boot_server(&project, http_port)?;
    let endpoint = format!("127.0.0.1:{grpc_port}");

    let result = drive_run_to_completion(&project, &endpoint);

    // Shutdown regardless of the verdict so the failure path reports the
    // assertion, not a leaked child.
    let term = Command::new("kill")
        .args(["-TERM", &server.id().to_string()])
        .status()?;
    assert!(term.success(), "failed to deliver SIGTERM");
    let exit_code = wait_for_exit(&mut server, EXIT_DEADLINE)?;
    result?;
    assert_eq!(exit_code, Some(0), "graceful drain must exit 0");
    Ok(())
}

/// Deploy, start with the README's example input, and poll `describe` until
/// the run completes with the expected greeting.
fn drive_run_to_completion(project: &Path, endpoint: &str) -> Result<(), TestError> {
    deploy_archive(project, endpoint)?;

    let output = common::run_cli(
        project,
        &[
            "--endpoint",
            endpoint,
            "start",
            "hello_demo",
            "--input",
            r#"{"name":"Ada"}"#,
        ],
    )?;
    let started = common::success_json(&output)?;
    let workflow_id = started["workflow_id"]
        .as_str()
        .ok_or("start must print the workflow id")?
        .to_owned();

    let deadline = Instant::now() + COMPLETION_DEADLINE;
    loop {
        let output = common::run_cli(project, &["--endpoint", endpoint, "describe", &workflow_id])?;
        let described = common::success_json(&output)?;
        let status = described["summary"]["status"]
            .as_str()
            .ok_or("describe must report the projected status")?
            .to_owned();
        if status == "Completed" {
            let rendered = described.to_string();
            if !rendered.contains("Hello, Ada!") {
                return Err(
                    format!("completed history must carry the typed greeting: {rendered}").into(),
                );
            }
            return Ok(());
        }
        if status != "Running" {
            return Err(format!("run reached unexpected terminal status {status}").into());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "run did not complete within {COMPLETION_DEADLINE:?}; last describe: {described}"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
