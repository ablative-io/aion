//! Live end-to-end proof for the stacked-dev example against real binaries:
//! build the three `.aion` archives from the committed Gleam source, boot
//! `aion server`, run the standalone Rust worker from
//! `examples/stacked-dev/worker/` (built against the published `aion-worker`
//! SDK) with fake-CLI shims as its entire `PATH`, start a `stacked_dev` run,
//! drive the `review_verdict` signal by hand, and assert the run completes
//! with the landed output. Every step drives a real process; the shims
//! intercept only at the `yg`/`norn`/`cargo`/`meridian` process boundary —
//! exactly how the example's hermetic Gleam suite tests the same contract.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

type TestError = Box<dyn std::error::Error>;

/// Runs the real `aion` binary with `args` from `current_dir` and captures
/// the output.
fn run_cli(current_dir: &Path, args: &[&str]) -> Result<Output, TestError> {
    Ok(Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(args)
        .current_dir(current_dir)
        .output()?)
}

/// Asserts a successful exit and returns stdout parsed as JSON.
fn success_json(output: &Output) -> Result<serde_json::Value, TestError> {
    if output.status.code() != Some(0) {
        return Err(format!(
            "expected success, got {:?}; stdout: {} stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

const BOOT_DEADLINE: Duration = Duration::from_secs(60);
const GRPC_DEADLINE: Duration = Duration::from_secs(30);
const PHASE_DEADLINE: Duration = Duration::from_secs(120);
const COMPLETION_DEADLINE: Duration = Duration::from_secs(120);
const EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// The branch the land step merges (the same value
/// the Gleam suite's shim uses).
const LANDED_BRANCH: &str = "stacked-dev-brief-7";
/// The tree parent the land step merges into.
const MERGED_INTO: &str = "main";

fn repo_root() -> Result<PathBuf, TestError> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

/// Reserve a loopback port by binding to port 0 and dropping the listener.
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
        if self.child.try_wait().map(|s| s.is_none()).unwrap_or(false) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Build the example's archives from source with `aion package --build`,
/// serialized with the same advisory flock the `examples_e2e` gate uses so
/// concurrent test binaries never race the example's `build/` directory.
fn build_example_archives(repo: &Path) -> Result<(), TestError> {
    let example = repo.join("examples/stacked-dev");
    let lock_dir = repo.join("target/example-build-locks");
    std::fs::create_dir_all(&lock_dir)?;
    let lock_file = std::fs::File::create(lock_dir.join("examples-stacked-dev.lock"))?;
    fs4::FileExt::lock(&lock_file).map_err(|error| format!("example build lock: {error}"))?;

    let output = run_cli(&example, &["package", ".", "--build"])?;
    let report = success_json(&output)?;
    let packaged: Vec<&str> = report["packages"]
        .as_array()
        .map(|packages| {
            packages
                .iter()
                .filter_map(|entry| entry["workflow_type"].as_str())
                .collect()
        })
        .unwrap_or_default();
    for expected in ["stacked_dev", "onatopp_dev", "gate"] {
        if !packaged.contains(&expected) {
            return Err(format!("package must report {expected}: {report}").into());
        }
    }
    for archive in ["stacked-dev.aion", "onatopp-dev.aion", "gate.aion"] {
        if !example.join(archive).is_file() {
            return Err(format!("packaging did not produce {archive}").into());
        }
    }
    Ok(())
}

/// Build the standalone worker crate (its own out-of-workspace package
/// consuming the published `aion-worker`) and return the binary path.
fn build_worker_binary(repo: &Path) -> Result<PathBuf, TestError> {
    let worker_dir = repo.join("examples/stacked-dev/worker");
    let status = Command::new("cargo")
        .arg("build")
        .current_dir(&worker_dir)
        .status()
        .map_err(|error| format!("failed to spawn cargo build for the worker: {error}"))?;
    if !status.success() {
        return Err(format!("worker `cargo build` failed with {status}").into());
    }
    let binary = worker_dir.join("target/debug/stacked-dev-worker");
    if !binary.is_file() {
        return Err(format!("worker binary missing at {}", binary.display()).into());
    }
    Ok(binary)
}

/// Write the dev-config the server boots from, mirroring the shared template
/// with reserved loopback ports. The store stays the relative `aion.db`,
/// which lands in `project` because the server runs from there.
fn write_server_config(project: &Path, http_port: u16, grpc_port: u16) -> Result<(), TestError> {
    let config = format!(
        r#"[server]
listen_address = "127.0.0.1:{http_port}"
grpc_address = "127.0.0.1:{grpc_port}"

[store]
backend = "libsql"
url = "aion.db"

[runtime]
query_timeout_ms = 10000

[websocket]
event_broadcast_capacity = 1024

[deploy]
enabled = true
max_archive_bytes = 16777216
max_inflated_bytes = 67108864
"#
    );
    std::fs::write(project.join("aion.toml"), config)?;
    Ok(())
}

/// Write one fake-CLI shim: records its argv to `<dir>/<name>.log`, then
/// runs `body`. Same skeleton as the example's Gleam test shims.
fn write_shim(dir: &Path, name: &str, body: &str) -> Result<(), TestError> {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let script = format!(
        "#!/bin/sh\nPATH=/usr/bin:/bin\necho \"$@\" >> \"{}/{name}.log\"\n{body}\n",
        dir.display()
    );
    std::fs::write(&path, script)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Install the happy-path shim set: yg provisions/passes, norn answers the
/// structured `DevResult` for dev and resume, cargo warm-builds clean, and
/// meridian acks the review then submits and lands.
fn write_shims(dir: &Path) -> Result<(), TestError> {
    write_shim(
        dir,
        "yg",
        r#"case "$1" in
  branch)
    case "$2" in
      add) exit 0 ;;
      provision) mkdir -p "$5"; exit 0 ;;
      merge) exit 0 ;;
      *) echo "unknown yg branch: $2" >&2; exit 64 ;;
    esac
    ;;
  graph)
    printf '%s\n' 'aion-core'
    exit 0
    ;;
  diagnostics)
    exit 0
    ;;
  *)
    echo "unknown yg subcommand: $1" >&2; exit 64
    ;;
esac"#,
    )?;
    write_shim(
        dir,
        "norn",
        r#"case "$2" in
  --session-id)
    printf '%s' '{"session_id":"shim","files_touched":["crates/aion-core/src/lib.rs"],"summary":"implemented the brief"}'
    ;;
  --resume)
    printf '%s' '{"session_id":"shim","files_touched":["crates/aion-core/src/lib.rs"],"summary":"applied feedback"}'
    ;;
  *)
    echo "unexpected norn invocation: $*" >&2
    exit 64
    ;;
esac"#,
    )?;
    write_shim(dir, "cargo", "exit 0")?;
    write_shim(
        dir,
        "meridian",
        r#"case "$1" in
  review)
    printf '%s' '{"request_id":"rev-1"}'
    ;;
  *)
    echo "unknown meridian subcommand: $1" >&2
    exit 64
    ;;
esac"#,
    )?;
    Ok(())
}

/// Boot `aion server --config aion.toml` from the project directory and wait
/// for the liveness probe.
fn boot_server(project: &Path, http_port: u16) -> Result<Child, TestError> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(["server", "--config", "aion.toml"])
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

/// Deploy one archive, retrying while the gRPC listener finishes binding.
fn deploy_archive(
    example: &Path,
    endpoint: &str,
    archive: &str,
    workflow_type: &str,
) -> Result<(), TestError> {
    let started = Instant::now();
    loop {
        let output = run_cli(example, &["--endpoint", endpoint, "deploy", archive])?;
        if output.status.code() == Some(0) {
            let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            if body["workflow_type"] != workflow_type {
                return Err(
                    format!("deploy of {archive} must report {workflow_type}: {body}").into(),
                );
            }
            return Ok(());
        }
        if started.elapsed() > GRPC_DEADLINE {
            return Err(format!(
                "deploy of {archive} did not succeed within {GRPC_DEADLINE:?}; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn stacked_dev_lands_through_the_real_worker_and_review_signal() -> Result<(), TestError> {
    let repo = repo_root()?;
    let example = repo.join("examples/stacked-dev");
    build_example_archives(&repo)?;
    let worker_binary = build_worker_binary(&repo)?;

    // Run state: server config + store, the shim PATH, and the repo the
    // workflow provisions its worktree under.
    let temp_dir = tempfile::tempdir()?;
    let project = temp_dir.path().join("server");
    let shim_dir = temp_dir.path().join("shims");
    let workflow_repo = temp_dir.path().join("repo");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&shim_dir)?;
    std::fs::create_dir_all(&workflow_repo)?;
    write_shims(&shim_dir)?;

    let http_port = reserve_port()?;
    let grpc_port = reserve_port()?;
    write_server_config(&project, http_port, grpc_port)?;
    let mut server = ChildGuard::new(boot_server(&project, http_port)?, "aion server");
    let endpoint = format!("127.0.0.1:{grpc_port}");

    let result = (|| -> Result<(), TestError> {
        deploy_archive(&example, &endpoint, "stacked-dev.aion", "stacked_dev")?;
        deploy_archive(&example, &endpoint, "onatopp-dev.aion", "onatopp_dev")?;
        deploy_archive(&example, &endpoint, "gate.aion", "gate")?;

        // The worker's entire PATH is the shim directory: the handlers really
        // shell out, the shims intercept, and anything unshimmed is genuinely
        // absent.
        let worker_child = Command::new(&worker_binary)
            .args(["--endpoint", &format!("http://127.0.0.1:{grpc_port}")])
            .env("PATH", &shim_dir)
            .current_dir(temp_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut worker = ChildGuard::new(worker_child, "stacked-dev-worker");

        let input = format!(
            r#"{{"repo_root":"{}","brief_id":"brief-7","reviewers":["sample-reviewer"],"base_ref":"main","placement":"local","isolation":"worktree","brief":"Implement the widget","design":"docs/design.md","checklist":"docs/checklist.md","stories":["story-1"],"verify_fix_cap":3,"review_cap":3,"round_backoff_ms":100,"review_deadline_ms":86400000}}"#,
            workflow_repo.display()
        );
        let workflow_id =
            start_run_once_the_worker_serves(&project, &endpoint, &input, &mut worker)?;

        wait_for_review_phase(&project, &endpoint, &workflow_id, &mut worker)?;

        let output = run_cli(
            &project,
            &[
                "--endpoint",
                &endpoint,
                "signal",
                &workflow_id,
                "review_verdict",
                "--payload",
                r#"{"decision":"approve"}"#,
            ],
        )?;
        success_json(&output)?;

        wait_for_landed_completion(&project, &endpoint, &workflow_id, &mut worker)?;

        // The provisioned worktree must really exist: the yg shim's
        // `branch provision` created it at the path the activity derived.
        let worktree = workflow_repo.join(".yggdrasil-worktrees/stacked-dev-brief-7");
        if !worktree.is_dir() {
            return Err(format!("provision never created {}", worktree.display()).into());
        }
        Ok(())
    })();

    // Shutdown regardless of the verdict so the failure path reports the
    // assertion, not a leaked child.
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

/// Poll the `stacked_dev_status` query until the run parks in the review
/// wait, then poll `describe` until the review request's ack (`rev-1`) is
/// durably recorded — the workflow is at or past the signal receive.
fn wait_for_review_phase(
    project: &Path,
    endpoint: &str,
    workflow_id: &str,
    worker: &mut ChildGuard,
) -> Result<(), TestError> {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        worker.require_alive()?;
        let output = run_cli(
            project,
            &[
                "--endpoint",
                endpoint,
                "query",
                workflow_id,
                "stacked_dev_status",
            ],
        )?;
        if output.status.code() == Some(0) {
            let answered: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            if answered["result"]["phase"] == "in_review" {
                break;
            }
        } else {
            // The query can fail transiently before the handler registers,
            // but a terminally-failed run will never serve it: surface the
            // recorded history immediately instead of burning the deadline.
            require_still_running(project, endpoint, workflow_id)?;
        }
        if Instant::now() > deadline {
            return Err(format!(
                "run never reached the in_review phase within {PHASE_DEADLINE:?}; last query: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        worker.require_alive()?;
        let output = run_cli(project, &["--endpoint", endpoint, "describe", workflow_id])?;
        let described = success_json(&output)?;
        if described.to_string().contains("rev-1") {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "the review request ack never appeared in history within {PHASE_DEADLINE:?}: {described}"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Start the `stacked_dev` run, tolerating exactly one failure mode: the
/// engine fails an activity terminally when NO worker serves its type, so a
/// run started before the worker's gRPC registration lands dies at
/// `provision_workspace`. There is no worker-listing API to gate on, so the
/// run itself is the readiness probe: while a started run fails with
/// "no connected worker", start a fresh one until the deadline. Any other
/// failure (and any later one) is reported verbatim by the phase wait. A
/// worker process that died (it can never register) fails immediately with
/// the worker's own output instead of burning the deadline on retries.
fn start_run_once_the_worker_serves(
    project: &Path,
    endpoint: &str,
    input: &str,
    worker: &mut ChildGuard,
) -> Result<String, TestError> {
    let deadline = Instant::now() + GRPC_DEADLINE;
    loop {
        worker.require_alive()?;
        let output = run_cli(
            project,
            &[
                "--endpoint",
                endpoint,
                "start",
                "stacked_dev",
                "--input",
                input,
            ],
        )?;
        let started = success_json(&output)?;
        let workflow_id = started["workflow_id"]
            .as_str()
            .ok_or("start must print the workflow id")?
            .to_owned();

        std::thread::sleep(Duration::from_millis(300));
        let output = run_cli(project, &["--endpoint", endpoint, "describe", &workflow_id])?;
        let described = success_json(&output)?;
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

/// Fail fast with the full recorded history when the run has already reached
/// a terminal status (a transiently-unregistered query handler keeps the run
/// in `Running`; anything else is a real failure to report verbatim).
fn require_still_running(
    project: &Path,
    endpoint: &str,
    workflow_id: &str,
) -> Result<(), TestError> {
    let output = run_cli(project, &["--endpoint", endpoint, "describe", workflow_id])?;
    let described = success_json(&output)?;
    let status = described["summary"]["status"]
        .as_str()
        .ok_or("describe must report the projected status")?;
    if status == "Running" {
        return Ok(());
    }
    Err(format!("run reached terminal status {status} before the review wait: {described}").into())
}

/// Poll `describe` until the run completes, then assert the landed output
/// (the shim's PR URL and merge commit) is durably recorded.
fn wait_for_landed_completion(
    project: &Path,
    endpoint: &str,
    workflow_id: &str,
    worker: &mut ChildGuard,
) -> Result<(), TestError> {
    let deadline = Instant::now() + COMPLETION_DEADLINE;
    loop {
        worker.require_alive()?;
        let output = run_cli(project, &["--endpoint", endpoint, "describe", workflow_id])?;
        let described = success_json(&output)?;
        let status = described["summary"]["status"]
            .as_str()
            .ok_or("describe must report the projected status")?
            .to_owned();
        if status == "Completed" {
            let rendered = described.to_string();
            if !rendered.contains(LANDED_BRANCH) || !rendered.contains(MERGED_INTO) {
                return Err(format!(
                    "completed history must carry the landed output ({LANDED_BRANCH}, {MERGED_INTO}): {rendered}"
                )
                .into());
            }
            return Ok(());
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
