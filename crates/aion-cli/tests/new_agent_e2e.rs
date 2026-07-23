//! Full from-scaffold proof for the `agent` template: `aion new agent` →
//! `gleam build` → `aion package` → build the agent-step worker crate → boot
//! `aion server` against the generated `aion.toml` → `aion deploy` → run a
//! trivial agent end to end through scout -> act -> verify -> review, twice:
//!
//!   * the human-review signal RESOLVES — an `agent_review` `approve` arrives,
//!     and the run completes with `disposition: applied`;
//!   * the human-review wait TIMES OUT — no signal arrives within the
//!     caller-chosen `review_timeout_ms`, and the run completes with
//!     `disposition: held` and the timeout reason.
//!
//! Every step drives the real `aion` binary and the real worker. The scaffold
//! bundles no agent runtime: the worker's trivial echo handlers prove the loop
//! runs without one (ADR-011).
//!
//! Runtime-gated: a missing `gleam` or `cargo` toolchain prints an explicit
//! skip line and returns `Ok(())`, so the suite passes on a host without the
//! Gleam toolchain rather than failing or silently lying.

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::TestError;

const BOOT_DEADLINE: Duration = Duration::from_secs(60);
const GRPC_DEADLINE: Duration = Duration::from_secs(30);
const PHASE_DEADLINE: Duration = Duration::from_secs(120);
const COMPLETION_DEADLINE: Duration = Duration::from_secs(120);
const EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// The project (and worker binary) name used throughout this gate.
const PROJECT: &str = "agent_demo";
/// A whole day, in milliseconds: long enough that the review wait stays open
/// for the approve case until the test sends the signal.
const LONG_REVIEW_MS: u64 = 86_400_000;
/// A short review deadline whose lapse the timeout case proves.
const SHORT_REVIEW_MS: u64 = 1_000;

/// Returns whether an executable resolves on the current `PATH`.
fn tool_on_path(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn reserve_port() -> Result<u16, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
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
    Err("process did not exit within the deadline".into())
}

/// Kills the child on every exit path so a failed assertion never leaks a
/// server or worker process.
struct ChildGuard {
    child: Child,
    name: &'static str,
}

impl ChildGuard {
    fn new(child: Child, name: &'static str) -> Self {
        Self { child, name }
    }

    /// Fails when the child has already exited (it must still be serving).
    fn require_alive(&mut self) -> Result<(), TestError> {
        if let Some(status) = self.child.try_wait()? {
            return Err(format!(
                "{} exited prematurely with {status}; output:\n{}",
                self.name,
                captured_output(&mut self.child)
            )
            .into());
        }
        Ok(())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.child.try_wait().is_ok_and(|s| s.is_none()) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Re-point the generated config at reserved loopback ports, mirroring the
/// hello-world gate. The store stays the generated relative `aion.db`.
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

/// Build the scaffolded agent-step worker against the workspace `aion-worker`
/// (the emitted manifest pins the published version; the test appends a
/// `[patch.crates-io]` so the gate builds against the source matching this
/// engine, exactly like the saga/dev-pipeline worker gates), repointing the
/// worker's hardcoded gRPC endpoint at the reserved port the server binds, and
/// return the built binary path. The emitted worker hardcodes the endpoint to
/// match `aion.toml` (no `--endpoint` flag, exactly like the saga worker), so
/// the gate substitutes the reserved port in source before building.
fn build_worker_binary(project: &Path, grpc_port: u16) -> Result<PathBuf, TestError> {
    let main_path = project.join("worker/src/main.rs");
    let main = std::fs::read_to_string(&main_path)?;
    let default_endpoint = "http://127.0.0.1:50051";
    if !main.contains(default_endpoint) {
        return Err(format!(
            "emitted worker must hardcode the dev gRPC endpoint ({default_endpoint}):\n{main}"
        )
        .into());
    }
    std::fs::write(
        &main_path,
        main.replace(default_endpoint, &format!("http://127.0.0.1:{grpc_port}")),
    )?;

    let manifest_path = project.join("worker/Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)?;
    let published = format!("aion-worker = \"{}\"", env!("CARGO_PKG_VERSION"));
    if !manifest.contains(&published) {
        return Err(format!(
            "emitted worker manifest must require the published SDK ({published}); got:\n{manifest}"
        )
        .into());
    }
    let workspace_worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aion-worker")
        .canonicalize()?;
    let patched = format!(
        "{manifest}\n[patch.crates-io]\naion-worker = {{ path = \"{}\" }}\n",
        workspace_worker.display()
    );
    std::fs::write(&manifest_path, patched)?;

    let status = Command::new("cargo")
        .arg("build")
        // The scaffolded project must build into its OWN target dir: an
        // inherited `CARGO_TARGET_DIR` (the workspace's shared target pile)
        // would strand the binary away from the path asserted below.
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(project.join("worker"))
        .status()
        .map_err(|error| format!("failed to spawn cargo build for the agent worker: {error}"))?;
    if !status.success() {
        return Err(format!("agent worker `cargo build` failed with {status}").into());
    }
    let binary = project.join(format!("worker/target/debug/{PROJECT}_worker"));
    if !binary.is_file() {
        return Err(format!("agent worker binary missing at {}", binary.display()).into());
    }
    Ok(binary)
}

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

fn deploy_archive(project: &Path, endpoint: &str) -> Result<(), TestError> {
    let started = Instant::now();
    loop {
        let output = common::run_cli(
            project,
            &["--endpoint", endpoint, "deploy", &format!("{PROJECT}.aion")],
        )?;
        if output.status.code() == Some(0) {
            let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            if body["workflow_type"] != PROJECT {
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

/// The run input for one agent run: three prompts plus a review deadline.
fn run_input(task_id: &str, review_timeout_ms: u64) -> String {
    serde_json::json!({
        "task_id": task_id,
        "scout_prompt": "survey",
        "act_prompt": "do",
        "verify_prompt": "check",
        "review_timeout_ms": review_timeout_ms,
    })
    .to_string()
}

/// Start a run, tolerating the one expected pre-registration failure: a run
/// started before the worker's gRPC registration lands dies at `scout` with
/// "no connected worker". The run is its own readiness probe; retry until the
/// worker serves or the deadline lapses.
fn start_run_once_the_worker_serves(
    project: &Path,
    endpoint: &str,
    input: &str,
    worker: &mut ChildGuard,
) -> Result<String, TestError> {
    let deadline = Instant::now() + GRPC_DEADLINE;
    loop {
        worker.require_alive()?;
        let output = common::run_cli(
            project,
            &["--endpoint", endpoint, "start", PROJECT, "--input", input],
        )?;
        let started = common::success_json(&output)?;
        let workflow_id = started["workflow_id"]
            .as_str()
            .ok_or("start must print the workflow id")?
            .to_owned();

        std::thread::sleep(Duration::from_millis(300));
        let output = common::run_cli(project, &["--endpoint", endpoint, "describe", &workflow_id])?;
        let described = common::success_json(&output)?;
        let rendered = described.to_string();
        let raced_registration = described["summary"]["status"] == "Failed"
            && rendered.contains("no connected worker for activity type");
        if !raced_registration {
            return Ok(workflow_id);
        }
        if Instant::now() > deadline {
            return Err(format!(
                "the worker never registered within {GRPC_DEADLINE:?}; last run: {rendered}"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Poll the `agent_status` query until the run parks in the review wait —
/// proof that scout -> act -> verify all ran and the durable receive is open.
fn wait_for_review_phase(
    project: &Path,
    endpoint: &str,
    workflow_id: &str,
    worker: &mut ChildGuard,
) -> Result<(), TestError> {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        worker.require_alive()?;
        let output = common::run_cli(
            project,
            &["--endpoint", endpoint, "query", workflow_id, "agent_status"],
        )?;
        if output.status.code() == Some(0) {
            let answered: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            if answered["result"]["stage"] == "awaiting_review" {
                return Ok(());
            }
        } else {
            require_still_running(project, endpoint, workflow_id)?;
        }
        if Instant::now() > deadline {
            return Err(format!(
                "run never reached awaiting_review within {PHASE_DEADLINE:?}; last query: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Fail fast with the full recorded history when the run already reached a
/// terminal status (a transiently-unregistered query handler keeps it
/// `Running`; anything else is a real failure to report verbatim).
fn require_still_running(
    project: &Path,
    endpoint: &str,
    workflow_id: &str,
) -> Result<(), TestError> {
    let output = common::run_cli(project, &["--endpoint", endpoint, "describe", workflow_id])?;
    let described = common::success_json(&output)?;
    let status = described["summary"]["status"]
        .as_str()
        .ok_or("describe must report the projected status")?;
    if status == "Running" {
        return Ok(());
    }
    Err(format!("run reached terminal status {status} before the review wait: {described}").into())
}

/// Poll `describe` until the run completes and return its rendered history,
/// asserting the terminal status is `Completed` (a held artifact is a
/// successful run, not a failure).
fn wait_for_completion(
    project: &Path,
    endpoint: &str,
    workflow_id: &str,
    worker: &mut ChildGuard,
) -> Result<String, TestError> {
    let deadline = Instant::now() + COMPLETION_DEADLINE;
    loop {
        worker.require_alive()?;
        let output = common::run_cli(project, &["--endpoint", endpoint, "describe", workflow_id])?;
        let described = common::success_json(&output)?;
        let status = described["summary"]["status"]
            .as_str()
            .ok_or("describe must report the projected status")?
            .to_owned();
        if status == "Completed" {
            return Ok(described.to_string());
        }
        if status != "Running" {
            return Err(
                format!("run reached unexpected terminal status {status}: {described}").into(),
            );
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

fn signal_review(
    project: &Path,
    endpoint: &str,
    workflow_id: &str,
    payload: &str,
) -> Result<Output, TestError> {
    common::run_cli(
        project,
        &[
            "--endpoint",
            endpoint,
            "signal",
            workflow_id,
            "agent_review",
            "--payload",
            payload,
        ],
    )
}

/// Drive the approve path: scout -> act -> verify -> review, signal `approve`,
/// and assert the run applies the artifact.
fn run_approve_case(
    project: &Path,
    endpoint: &str,
    worker: &mut ChildGuard,
) -> Result<(), TestError> {
    let input = run_input("approve-task", LONG_REVIEW_MS);
    let workflow_id = start_run_once_the_worker_serves(project, endpoint, &input, worker)?;
    wait_for_review_phase(project, endpoint, &workflow_id, worker)?;

    let output = signal_review(
        project,
        endpoint,
        &workflow_id,
        r#"{"decision":"approve","reviewer":"ada"}"#,
    )?;
    common::success_json(&output)?;

    let rendered = wait_for_completion(project, endpoint, &workflow_id, worker)?;
    for marker in [
        r#""disposition":"applied""#,
        r#""reviewed_by":"ada""#,
        "approved by ada",
        // Proof the whole loop ran: each step's echoed artifact is recorded.
        "scouted(survey)",
        "acted(do",
        "verified(check",
    ] {
        if !rendered.contains(marker) {
            return Err(format!("approved run history must carry {marker}: {rendered}").into());
        }
    }
    Ok(())
}

/// Drive the timeout path: scout -> act -> verify -> review, send NO signal,
/// and assert the review wait lapses and the run holds the artifact.
fn run_timeout_case(
    project: &Path,
    endpoint: &str,
    worker: &mut ChildGuard,
) -> Result<(), TestError> {
    let input = run_input("timeout-task", SHORT_REVIEW_MS);
    let workflow_id = start_run_once_the_worker_serves(project, endpoint, &input, worker)?;

    // No signal is ever sent: the caller-chosen deadline must lapse on its own
    // and complete the run as held.
    let rendered = wait_for_completion(project, endpoint, &workflow_id, worker)?;
    for marker in [r#""disposition":"held""#, "review timed out after 1000ms"] {
        if !rendered.contains(marker) {
            return Err(format!("timed-out run history must carry {marker}: {rendered}").into());
        }
    }
    if rendered.contains(r#""disposition":"applied""#) {
        return Err(format!("a timed-out run must not apply the artifact: {rendered}").into());
    }
    Ok(())
}

#[test]
fn scaffolded_agent_runs_scout_act_verify_review_with_resolve_and_timeout() -> Result<(), TestError>
{
    if !tool_on_path("gleam") || !tool_on_path("cargo") {
        eprintln!(
            "skipping new_agent_e2e: the agent scaffold gate needs both `gleam` and `cargo` on \
             PATH (gleam build + the worker build); one is absent, so this run is skipped"
        );
        return Ok(());
    }

    let temp_dir = tempfile::tempdir()?;
    let (project, report) = common::scaffold_project(
        temp_dir.path(),
        PROJECT,
        &["--template", "agent", "--worker", "rust"],
    )?;
    if report["template"] != "agent" || report["worker"] != "rust" {
        return Err(
            format!("scaffold report must name the agent template and worker: {report}").into(),
        );
    }

    common::patch_aion_flow_to_workspace(&project)?;
    common::gleam_build(&project)?;
    common::package_project(&project, PROJECT)?;

    let http_port = reserve_port()?;
    let grpc_port = reserve_port()?;
    let worker_binary = build_worker_binary(&project, grpc_port)?;
    repoint_config_ports(&project, http_port, grpc_port)?;
    let mut server = ChildGuard::new(boot_server(&project, http_port)?, "aion server");
    let endpoint = format!("127.0.0.1:{grpc_port}");

    let result = (|| -> Result<(), TestError> {
        deploy_archive(&project, &endpoint)?;

        let worker_child = Command::new(&worker_binary)
            .current_dir(&project)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut worker = ChildGuard::new(worker_child, "agent worker");

        run_approve_case(&project, &endpoint, &mut worker)?;
        run_timeout_case(&project, &endpoint, &mut worker)?;
        Ok(())
    })();

    let term = Command::new("kill")
        .args(["-TERM", &server.child.id().to_string()])
        .status()?;
    if !term.success() {
        return Err("failed to deliver SIGTERM to the server".into());
    }
    let exit_code = wait_for_exit(&mut server.child, EXIT_DEADLINE)?;
    result?;
    if exit_code != Some(0) {
        return Err(format!("graceful drain must exit 0, got {exit_code:?}").into());
    }
    Ok(())
}
