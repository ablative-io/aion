//! Task #148 — prove a PARKED durable-timer workflow resumes under a TRUE
//! OS-process `kill -9`, end to end, across a real multi-node `aion server`
//! cluster.
//!
//! ## The gap this closes
//!
//! The #119 fix (on main: abort armed live-wheel timers on engine shutdown)
//! closed the GRACEFUL-shutdown failover race for parked durable timers, and the
//! in-process `adoption_parked_timer_e2e` gate proves a survivor's
//! `Engine::adopt_shards` re-arms a parked timer. But both of those exercise a
//! GRACEFUL teardown of the owner (an orderly `shutdown()` / responder join).
//!
//! The Sydney demo's dramatic beat is a real `kill -9` of a separate OS process:
//! the owner dies ENTIRELY, no graceful abort runs, no shutdown hook fires. In
//! that path the survivor's adoption-arming (`recover_timers_on_startup`, invoked
//! from `Engine::adopt_shards` via the cluster supervisor) must carry the resume
//! ALONE — a path plausible but, before this test, UNVERIFIED across real
//! processes.
//!
//! ## Shape (modelled on `lsub5b_osproc_kill9_failover_e2e`)
//!
//!   1. boot a 3-node haematite cluster of REAL `aion server` processes (quorum
//!      needs >= 3 so a 2-node survivor majority can re-elect); node i owns shard
//!      i; the killed owner's shard is adopted by exactly one designated survivor.
//!      Outbox transport is `grpc` and the workload uses NO activities, so no
//!      liminal worker is needed — this isolates the durable TIMER path;
//!   2. start the `sleep_query` workflow (the fixture the in-process parked-timer
//!      gate uses) with a multi-second durable sleep, retried until it lands on
//!      node 0's shard, and confirm it is `Running` (parked on its durable timer,
//!      `TimerStarted` recorded, no `TimerFired`) BEFORE the kill;
//!   3. `kill -9` node 0 (the owner). The process dies entirely — no graceful
//!      abort, no shutdown hook;
//!   4. a survivor's `ClusterSupervisor` auto-adopts shard 0 (shipped library
//!      code) and its adoption-arming re-arms the parked durable timer;
//!   5. ASSERT on real observables over a SURVIVOR's gRPC (`aion describe`): the
//!      workflow reaches `Completed` with EXACTLY ONE `TimerFired` and EXACTLY
//!      ONE `WorkflowCompleted` — the timer fired and the workflow completed
//!      exactly-once across the hard kill.
//!
//! All failover intelligence lives in shipped library code (the
//! `ClusterSupervisor`, `Engine::adopt_shards`, and `recover_timers_on_startup`);
//! this test only boots processes, drives start/describe over the CLI,
//! `kill -9`s, and polls real observables. No sleeps gate the assertions.
//!
//! ## Why a self-contained harness (not the shared `common/osproc.rs`)
//!
//! The sibling kill-9 gate's shared `common/{osproc,aion_cli}.rs` carry
//! liminal-worker + outbox-fixture helpers this timer-only gate never uses;
//! `#[path]`-including them here would trip `dead_code` under the workspace's
//! `-D warnings` (the crate has no `#![allow(dead_code)]` convention for shared
//! test modules). This file therefore inlines the small process/CLI helpers it
//! needs, keeping the convention clean without touching the shared modules.
//!
//! ## CURRENT STATUS — GREEN positive failover regression guard (#148, fixed by #157)
//!
//! This gate PASSES end to end (exactly-once resume, ~15s). It once REPRODUCED a
//! real kill-9 failover quorum bug (the symptom below), which #157 FIXED; the
//! gate now stands as a positive regression guard that the hard-kill parked-timer
//! resume keeps working. It is `#[ignore]`d only because it is slow (spins up a
//! 3-node cluster + a hard kill), so the DEFAULT `cargo test` suite is unaffected.
//!
//! ### The bug this once reproduced, and the #157 fix
//!
//! Before #157, after `kill -9` of node 0 the survivor's `ClusterSupervisor`
//! detected the peer-down and called `Engine::adopt_shards([0])`, but adoption
//! then failed inside the parked-timer re-arm step:
//!
//! ```text
//! cluster supervisor failed to adopt a downed peer's shards; will retry
//!   error: timer recovery failed: timer recovery fire operation failed:
//!     ... workflow recorder failed: ... haematite database error:
//!     consistency requirement failed: timed out after 5s waiting for quorum:
//!     required 2, acknowledged 1
//! ```
//!
//! The re-armed durable timer fired and recovery tried to RECORD `TimerFired` —
//! a quorum write — but the adopted shard's write membership still expected an
//! ack from the now-dead owner and never enlisted the live survivor, so the
//! 2-node survivor majority only self-acked (1 < 2). This was a GENERAL
//! post-kill-9 quorum-membership failure (the sibling fan-out gate
//! `lsub5b_osproc_kill9_failover_e2e` died on the identical underlying error one
//! step later); the timer path merely surfaced it one step EARLIER, inside
//! `adopt_shards`.
//!
//! #157 fixed the root cause by forwarding the per-shard failover seam through
//! the store decorators so an adopted shard's write membership drops the dead
//! owner and enlists the live survivor. Quorum is now reachable on the adopted
//! shard, `adopt_shards` succeeds, the re-armed timer's `TimerFired` commits, and
//! the workflow resumes and completes exactly-once across the hard kill.
//!
//! ## Running
//!
//! Ignored (slow: spawns a 3-node `aion server` cluster + a hard-kill failover):
//! ```text
//! cargo test -p aion-cli --features haematite-backend,liminal-transport \
//!   --test parked_timer_osproc_kill9_failover_e2e -- --ignored --nocapture
//! ```

#![cfg(all(unix, feature = "haematite-backend", feature = "liminal-transport"))]

use std::fmt::Write as _;
use std::io::{Read, Write as _IoWrite};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Error type used across the harness.
type TestError = Box<dyn std::error::Error>;

// ---------------------------------------------------------------------------
// Tunables (generous but bounded; kill-9 detection + election + adoption + the
// re-armed timer all take seconds).
// ---------------------------------------------------------------------------

/// Number of nodes: haematite `quorum_size(3) = 2`, so a 3-node cluster losing
/// one leaves a 2-node majority that CAN re-elect and adopt the dead shard.
const NODE_COUNT: usize = 3;
/// The library log line a survivor's `ClusterSupervisor::tick` emits on
/// auto-adoption — the load-bearing proof the LIBRARY did the failover.
const ADOPT_LINE: &str = "adopted a downed peer's shards (SS-5b auto-failover)";
/// Stagger between node boots: sidesteps the documented beamr simultaneous-
/// connect boot race. Pure launch ordering; no failover logic.
const BOOT_STAGGER: Duration = Duration::from_secs(3);
/// Deadline for a spawned server to answer its liveness probe.
const BOOT_DEADLINE: Duration = Duration::from_secs(90);
/// Deadline for a reaped child to exit.
const EXIT_DEADLINE: Duration = Duration::from_secs(30);
/// Deadline for the cluster to fully boot (all supervisors commissioned).
const CLUSTER_BOOT_DEADLINE: Duration = Duration::from_secs(40);
/// Deadline for the post-kill failover to complete the workflow.
const FAILOVER_DEADLINE: Duration = Duration::from_secs(90);
/// Deadline for a `start` to land on node 0's shard (it is retried until it does).
const START_DEADLINE: Duration = Duration::from_secs(30);
/// The durable sleep the parked workflow takes. Long enough that the workflow is
/// reliably still PARKED (no `TimerFired`) when node 0 is killed, so the kill
/// genuinely interrupts a workflow waiting on a durable timer. If the absolute
/// deadline has already elapsed by the time a survivor re-arms it, the re-armed
/// wheel fires it promptly — the gate is whether adoption re-arms it AT ALL.
const SLEEP_MS: u64 = 6_000;

/// The node index that is killed (it always owns shard 0).
const OWNER_INDEX: usize = 0;
/// The node index designated as the SOLE adopter of the killed owner's shard, so
/// exactly one survivor adopts it. Every node still lists every peer for the
/// distribution mesh + quorum; only the killed owner's `owned_shards` is declared
/// exclusively here.
const ADOPTER_INDEX: usize = 1;

// ---------------------------------------------------------------------------
// Generic poll helpers (never a bare sleep gating an assertion).
// ---------------------------------------------------------------------------

/// Poll `predicate` until it returns true or `deadline` elapses; returns whether
/// it became true.
fn wait_until<F: FnMut() -> bool>(deadline: Duration, mut predicate: F) -> bool {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Whether `path` exists and its contents contain `needle` (a server log line).
fn file_contains(path: &Path, needle: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|contents| contents.contains(needle))
}

/// Reserve a loopback port by binding to 0 and dropping the listener.
fn reserve_port() -> Result<u16, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

// ---------------------------------------------------------------------------
// CLI client helpers (drive the real `aion` binary over gRPC).
// ---------------------------------------------------------------------------

/// Run `aion <args>` and return parsed JSON stdout, or `None` on any failure (a
/// fenced start, an unreachable endpoint) so the caller can retry/poll.
fn aion_json(args: &[&str]) -> Option<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Start the `sleep_query` workflow (parks on a durable timer) against `endpoint`
/// with the given sleep, returning its workflow id on success. The input is the
/// `{ "sleep_ms": N }` document the fixture's input codec decodes — the same
/// shape the in-process parked-timer gate passes through `Payload::from_json`.
fn try_start_sleeper(endpoint: &str, sleep_ms: u64) -> Option<String> {
    let input = format!("{{\"sleep_ms\":{sleep_ms}}}");
    let value = aion_json(&[
        "start",
        "sleep_query",
        "--input",
        &input,
        "--endpoint",
        endpoint,
    ])?;
    value
        .get("workflow_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Describe `workflow_id` over `endpoint`, returning the parsed JSON.
fn describe(endpoint: &str, workflow_id: &str) -> Option<Value> {
    aion_json(&["describe", workflow_id, "--endpoint", endpoint])
}

/// The `summary.status` string from a describe payload, if present.
fn status_of(description: &Value) -> Option<&str> {
    description
        .get("summary")
        .and_then(|summary| summary.get("status"))
        .and_then(Value::as_str)
}

/// Count history events of a given `type` in a describe payload. Events serialize
/// as `{"type": "...", ...}` (the `Event` enum's `tag = "type"`), so this reads
/// the exactly-once observable for any terminal — `TimerFired` and
/// `WorkflowCompleted` here.
fn event_type_count(description: &Value, event_type: &str) -> usize {
    description
        .get("history")
        .and_then(Value::as_array)
        .map_or(0, |events| {
            events
                .iter()
                .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
                .count()
        })
}

// ---------------------------------------------------------------------------
// Fixture archive build (from committed Gleam source — never a stale prebuilt).
// ---------------------------------------------------------------------------

/// Build the `sleep_query` fixture (a workflow that PARKS on a durable timer)
/// from its committed Gleam source and return a fresh `.aion` archive inside
/// `dir`, ready to drop into a node's `workflow_packages`. Mirrors the
/// from-source build philosophy of `crates/aion/tests/common/example_build.rs`:
/// it runs `gleam build` then the real `aion package`, then copies the produced
/// archive into the test's temp dir, so the cluster loads the SAME `sleep_query`
/// beam the in-process `adoption_parked_timer_e2e` gate proves. A missing `gleam`
/// CLI FAILS the gate loudly by design — never a skip.
fn build_sleep_query_archive(dir: &Path) -> Result<PathBuf, TestError> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aion/tests/fixtures/sleep_query")
        .canonicalize()?;

    let gleam = Command::new("gleam")
        .arg("build")
        .current_dir(&fixture)
        .status()
        .map_err(|error| {
            format!(
                "the parked-timer OS-process gate requires the `gleam` CLI on PATH \
                 (failed to spawn `gleam build` in {}: {error}); this gate fails \
                 loudly by design — never reintroduce a skip",
                fixture.display()
            )
        })?;
    if !gleam.success() {
        return Err(format!("`gleam build` failed in {} with {gleam}", fixture.display()).into());
    }

    let package = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(["package", "."])
        .current_dir(&fixture)
        .status()?;
    if !package.success() {
        return Err(format!(
            "`aion package .` failed in {} with {package}",
            fixture.display()
        )
        .into());
    }

    // `workflow.toml` declares `output = "sleep-query.aion"` (hyphenated).
    let built = fixture.join("sleep-query.aion");
    if !built.is_file() {
        return Err(format!(
            "aion package did not produce the declared archive {}",
            built.display()
        )
        .into());
    }
    let dest = dir.join("sleep-query.aion");
    std::fs::copy(&built, &dest)?;
    Ok(dest)
}

// ---------------------------------------------------------------------------
// Process lifecycle.
// ---------------------------------------------------------------------------

/// A booted node: its child process, ports, and log path.
struct Node {
    child: Child,
    http_port: u16,
    grpc_port: u16,
    log_path: PathBuf,
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

/// Block until `http_get_live` answers 200 or the boot deadline elapses. Surfaces
/// the child's output if it exits during boot.
fn wait_for_liveness(child: &mut Child, http_port: u16) -> Result<(), TestError> {
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

/// Reap every node so a failed assertion never leaks a cluster.
fn reap_all(nodes: Vec<Node>) {
    for node in nodes {
        reap(node.child);
    }
}

// ---------------------------------------------------------------------------
// Cluster config.
// ---------------------------------------------------------------------------

/// Emit one node's TOML config: an N-node haematite cluster with a `grpc` outbox
/// (the parked workflow has no activities, so the outbox never dispatches — this
/// isolates the durable-timer path and needs no liminal worker). Node `index`
/// owns shard `index`; the killed owner ([`OWNER_INDEX`]) is declared adoptable
/// ONLY in the designated adopter's config ([`ADOPTER_INDEX`]).
fn write_node_config(
    dir: &Path,
    index: usize,
    package: &Path,
    ports: &[(u16, u16)],
) -> Result<PathBuf, TestError> {
    let (http_port, grpc_port) = ports[index];
    let data_dir = dir.join(format!("data{index}"));
    let bind_port = 7100 + index;

    let mut members = String::new();
    for node in 0..ports.len() {
        let _ = write!(members, "\"node-{node}@127.0.0.1\", ");
    }
    let members = members.trim_end_matches(", ");

    let mut peers = String::new();
    for (peer, &(_, peer_grpc)) in ports.iter().enumerate() {
        if peer == index {
            continue;
        }
        let owned = if peer == OWNER_INDEX && index != ADOPTER_INDEX {
            String::new()
        } else {
            peer.to_string()
        };
        let _ = write!(
            peers,
            "\n[[store.cluster.peers]]\nname = \"node-{peer}@127.0.0.1\"\n\
             address = \"127.0.0.1:{peer_bind}\"\n\
             grpc_address = \"127.0.0.1:{peer_grpc}\"\n\
             owned_shards = [{owned}]\n",
            peer_bind = 7100 + peer,
        );
    }

    let config = format!(
        r#"workflow_packages = ["{package}"]

[server]
listen_address = "127.0.0.1:{http_port}"
grpc_address = "127.0.0.1:{grpc_port}"

[store]
backend = "haematite"
data_dir = "{data_dir}"
shard_count = {node_count}
owned_shards = [{index}]

[store.cluster]
node_id = "node-{index}@127.0.0.1"
bind_address = "127.0.0.1:{bind_port}"
members = [{members}]
failover_poll_interval_ms = 500
failover_confirmations = 3
{peers}
[runtime]
query_timeout_ms = 10000

[namespaces]
default = "default"

[websocket]
event_broadcast_capacity = 1024
cluster_broadcast_capacity = 64

[outbox]
enabled = true
poll_interval_ms = 20
batch_size = 16
max_attempts = 5
backoff_base_ms = 50
backoff_multiplier = 2
backoff_max_ms = 1000
transport = "grpc"
"#,
        package = package.display(),
        data_dir = data_dir.display(),
        node_count = ports.len(),
    );
    let path = dir.join(format!("node{index}.toml"));
    std::fs::write(&path, config)?;
    Ok(path)
}

/// Spawn one `aion server` OS process from `config`, logging to `log_path`.
fn boot_node(config: &Path, log_path: &Path, ports: (u16, u16)) -> Result<Node, TestError> {
    let log = std::fs::File::create(log_path)?;
    let stderr = log.try_clone()?;
    let child = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(["server", "--config", &config.to_string_lossy()])
        .env("RUST_LOG", "info")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()?;
    Ok(Node {
        child,
        http_port: ports.0,
        grpc_port: ports.1,
        log_path: log_path.to_path_buf(),
    })
}

/// Boot the N-node cluster, wait for every node's liveness probe, and confirm
/// each node commissioned its cluster supervisor (the auto-adoption driver).
/// Reaps any already-booted nodes on failure so a partial boot never leaks.
fn boot_cluster(dir: &Path, package: &Path, ports: &[(u16, u16)]) -> Result<Vec<Node>, TestError> {
    let mut nodes: Vec<Node> = Vec::with_capacity(ports.len());
    for index in 0..ports.len() {
        let config = match write_node_config(dir, index, package, ports) {
            Ok(config) => config,
            Err(error) => {
                reap_all(nodes);
                return Err(error);
            }
        };
        let log_path = dir.join(format!("node{index}.log"));
        match boot_node(&config, &log_path, ports[index]) {
            Ok(node) => nodes.push(node),
            Err(error) => {
                reap_all(nodes);
                return Err(error);
            }
        }
        if index + 1 < ports.len() {
            std::thread::sleep(BOOT_STAGGER);
        }
    }

    for index in 0..nodes.len() {
        let http_port = nodes[index].http_port;
        if let Err(error) = wait_for_liveness(&mut nodes[index].child, http_port) {
            reap_all(nodes);
            return Err(format!("node {index} failed to boot: {error}").into());
        }
    }
    let supervisors_up = wait_until(CLUSTER_BOOT_DEADLINE, || {
        nodes
            .iter()
            .all(|node| file_contains(&node.log_path, "commissioned"))
    });
    if supervisors_up {
        Ok(nodes)
    } else {
        reap_all(nodes);
        Err("cluster supervisors did not commission within the boot deadline".into())
    }
}

// ---------------------------------------------------------------------------
// The gate.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "slow: spawns a 3-node aion-server cluster + a hard-kill failover; \
            passes since #157; run with `-- --ignored --nocapture` (and \
            KILL9_KEEP_LOGS=<dir> to keep server logs)."]
fn osproc_kill9_parked_timer_resumes_and_completes_exactly_once() -> Result<(), TestError> {
    let temp = tempfile::tempdir()?;
    let package = build_sleep_query_archive(temp.path())?;

    // Reserve every port up front so configs can cross-reference peers' grpc.
    let mut ports: Vec<(u16, u16)> = Vec::with_capacity(NODE_COUNT);
    for _ in 0..NODE_COUNT {
        ports.push((reserve_port()?, reserve_port()?));
    }

    let mut nodes = boot_cluster(temp.path(), &package, &ports)?;

    let result = drive_failover(&mut nodes, &ports);
    if result.is_err() {
        dump_logs_if_requested(temp.path());
    }
    reap_all(nodes);
    result
}

/// On failure, copy the whole work dir (node logs + configs) to the path in
/// `KILL9_KEEP_LOGS`, for post-mortem diagnosis. No-op when unset.
fn dump_logs_if_requested(dir: &Path) {
    let Ok(dest) = std::env::var("KILL9_KEEP_LOGS") else {
        return;
    };
    let dest = Path::new(&dest);
    let _ = std::fs::create_dir_all(dest);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let _ = std::fs::copy(&path, dest.join(entry.file_name()));
            }
        }
    }
}

/// Drive the parked-timer start, the kill-9, and the survivor-side exactly-once
/// assertion. `nodes[0]` is the owner that gets killed; the remainder survive.
fn drive_failover(nodes: &mut Vec<Node>, ports: &[(u16, u16)]) -> Result<(), TestError> {
    let node0_grpc = format!("http://127.0.0.1:{}", ports[0].1);
    let workflow_id = start_on_node0(&node0_grpc)?;
    confirm_parked_before_kill(&node0_grpc, &workflow_id)?;

    // kill -9 node 0: the owner of shard 0. The process dies ENTIRELY — no
    // graceful abort, no shutdown hook. The survivor's adoption-arming must carry
    // the parked-timer resume alone.
    let owner = nodes.remove(0);
    let owner_pid = owner.child.id();
    let _: Result<std::process::ExitStatus, std::io::Error> = Command::new("kill")
        .args(["-9", &owner_pid.to_string()])
        .status();
    reap(owner.child);

    // A survivor's ClusterSupervisor must auto-adopt shard 0 (library failover).
    let adopted = wait_until(FAILOVER_DEADLINE, || {
        nodes
            .iter()
            .any(|node| file_contains(&node.log_path, ADOPT_LINE))
    });
    if !adopted {
        return Err("no survivor logged the SS-5b auto-adoption after the kill".into());
    }

    prove_resume_exactly_once(nodes, &workflow_id)
}

/// Start `sleep_query` so it lands on shard 0 (node 0's shard). An unsteered start
/// to node 0 lands on a locally-owned shard or is fenced; retry until it lands
/// (there is no cross-shard start routing). Returns the workflow id.
fn start_on_node0(node0_grpc: &str) -> Result<String, TestError> {
    let mut workflow_id = String::new();
    let landed = wait_until(START_DEADLINE, || {
        match try_start_sleeper(node0_grpc, SLEEP_MS) {
            Some(id) => {
                workflow_id = id;
                true
            }
            None => false,
        }
    });
    if landed {
        Ok(workflow_id)
    } else {
        Err("sleep_query never landed on node 0's shard".into())
    }
}

/// Confirm the workflow is genuinely PARKED on its durable timer (Running, with a
/// `TimerStarted` but NOT yet a `TimerFired`) before the kill, so the kill interrupts
/// a workflow waiting on a durable timer. A race to Completed before we observe
/// the parked state is a test FAILURE, not a silent pass.
fn confirm_parked_before_kill(node0_grpc: &str, workflow_id: &str) -> Result<(), TestError> {
    let parked = wait_until(FAILOVER_DEADLINE, || {
        let Some(description) = describe(node0_grpc, workflow_id) else {
            return false;
        };
        let running = status_of(&description) == Some("Running");
        let timer_started = event_type_count(&description, "TimerStarted") >= 1;
        let timer_fired = event_type_count(&description, "TimerFired") >= 1;
        running && timer_started && !timer_fired
    });
    if parked {
        return Ok(());
    }
    let last = describe(node0_grpc, workflow_id);
    let status = last.as_ref().and_then(status_of).unwrap_or("<none>");
    Err(format!(
        "sleep_query never observed PARKED-on-timer (Running + TimerStarted, no TimerFired) \
         on node 0 before the kill (last status: {status:?}); the kill would not have \
         interrupted a parked durable timer"
    )
    .into())
}

/// THE PROOF: read over a SURVIVOR's gRPC. The adopted parked workflow must
/// re-arm its durable timer, FIRE it, and reach Completed — exactly-once across
/// the hard kill (`TimerFired` == 1, `WorkflowCompleted` == 1).
fn prove_resume_exactly_once(nodes: &[Node], workflow_id: &str) -> Result<(), TestError> {
    let survivor_grpc: Vec<String> = nodes
        .iter()
        .map(|node| format!("http://127.0.0.1:{}", node.grpc_port))
        .collect();
    let mut final_description: Option<Value> = None;
    let completed = wait_until(FAILOVER_DEADLINE, || {
        for endpoint in &survivor_grpc {
            if let Some(description) = describe(endpoint, workflow_id) {
                if status_of(&description) == Some("Completed") {
                    final_description = Some(description);
                    return true;
                }
            }
        }
        false
    });
    if completed {
        check_exactly_once(final_description.as_ref())
    } else {
        Err(
            "the parked workflow did not reach Completed on any survivor after the kill-9 \
             failover (its durable timer was not re-armed on adoption)"
                .into(),
        )
    }
}

/// Assert the completed run fired its durable timer exactly once and completed
/// exactly once — exactly-once parked-timer resume across the hard kill.
fn check_exactly_once(description: Option<&Value>) -> Result<(), TestError> {
    let description =
        description.ok_or_else(|| TestError::from("missing completed describe payload"))?;
    let fired = event_type_count(description, "TimerFired");
    if fired != 1 {
        return Err(
            format!("exactly-once violated: expected exactly 1 TimerFired, found {fired}").into(),
        );
    }
    let completed = event_type_count(description, "WorkflowCompleted");
    if completed != 1 {
        return Err(format!(
            "exactly-once violated: expected exactly 1 WorkflowCompleted, found {completed}"
        )
        .into());
    }
    Ok(())
}
