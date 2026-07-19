//! LSUB-5-B kill-9 reconnect-to-survivor failover (G-1, #112): a REAL
//! multi-OS-process hard-kill liminal-PUSH failover that PASSES end to end.
//!
//! ## What this proves (the gap the connect-half gate stopped at)
//!
//! Each deployed `aion server` hosts its OWN liminal listener backed by its OWN
//! per-process `ConnectedWorkerRegistry`. Before G-1 the worker dialed ONE
//! address and exited on the first transport error, so when the owner of a
//! fan-out's shard was `kill -9`'d the worker could not migrate to the survivor
//! that adopted the shard — a full kill-9 liminal failover was impossible across
//! real OS processes. G-1 gave the worker a STATIC candidate-address list and a
//! redial-on-drop loop (`aion_worker::serve_with_redial`) that re-runs the
//! in-band `WorkerRegister`/`Ack`, re-registering in the survivor's registry.
//!
//! This test exercises that capability over REAL processes, no in-process
//! shortcuts (the in-process LSUB-5 capstone only achieves liminal failover by
//! SHARING one listener+registry across both nodes — which does not exist across
//! OS processes):
//!
//!   1. boot a 3-node haematite cluster of REAL `aion server` processes (quorum
//!      needs >= 3: a 2-node survivor can never re-elect), each hosting its own
//!      liminal listener with `outbox.transport = liminal`; node i owns shard i;
//!   2. start ONE OS-process liminal worker with ALL THREE liminal addresses as
//!      candidates — it dials node 0 first and registers there;
//!   3. start the `collect_four` fan-out workflow so it lands on shard 0 (node
//!      0's shard); node 0's outbox dispatcher pushes fan-out activities to the
//!      worker over the liminal connection (a dispatch in flight);
//!   4. `kill -9` node 0 (the owner of shard 0 and the worker's connected
//!      server). The worker's connection drops; it redials the NEXT candidate
//!      and RE-REGISTERS in a survivor's registry;
//!   5. the survivors' `ClusterSupervisor`s auto-adopt shard 0 (shipped library
//!      code), and a survivor's outbox dispatcher re-dispatches the remaining
//!      fan-out rows to the re-registered worker;
//!   6. ASSERT on real observables read over a SURVIVOR's gRPC (`aion describe`):
//!      the workflow reaches `Completed`, and its history has EXACTLY ONE
//!      `ActivityCompleted` terminal per ordinal (4 total) — exactly-once, the
//!      `record_fan_out_completion` dedup absorbing the owner's lost wave plus
//!      the survivor's redelivery. No sleeps gate the assertions.
//!
//! All failover intelligence lives in shipped library code (the SS-5b
//! `ClusterSupervisor` + `Engine::adopt_shards` + the worker's `serve_with_redial`
//! redial loop); this test only boots processes, drives start/describe over the
//! CLI, `kill -9`s, and polls real observables.
//!
//! ## Running
//!
//! Ignored (slow, spawns 4 OS processes + a 3-node cluster):
//! ```text
//! cargo test -p aion-cli --features haematite-backend,liminal-transport \
//!   --test lsub5b_osproc_kill9_failover_e2e -- --ignored --nocapture
//! ```

#![cfg(all(unix, feature = "haematite-backend", feature = "liminal-transport"))]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

#[path = "common/aion_cli.rs"]
mod aion_cli;
#[path = "common/osproc.rs"]
mod osproc;

use aion_cli::{activity_completed_count, describe, status_of, try_start};
use osproc::{
    TestError, build_worker_binary, reap, reserve_port, wait_for_liveness, write_package_archive,
};

/// Poll `predicate` until it returns true or `deadline` elapses; returns whether
/// it became true. Used for log/store observables (never a bare sleep).
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

/// Number of nodes: haematite `quorum_size(3) = 2`, so a 3-node cluster losing
/// one leaves a 2-node majority that CAN re-elect and adopt the dead shard. A
/// 2-node cluster could not (a lone survivor never forms a majority).
const NODE_COUNT: usize = 3;
/// Fan-out arity of the `collect_four` fixture (one `ActivityCompleted` terminal
/// per ordinal on success).
const FAN_OUT: usize = 4;
/// The library log line a survivor's `ClusterSupervisor::tick` emits on
/// auto-adoption — the load-bearing proof the LIBRARY did the failover.
const ADOPT_LINE: &str = "adopted a downed peer's shards (SS-5b auto-failover)";
/// Stagger between node boots: sidesteps the documented beamr simultaneous-
/// connect boot race (a benign `SimultaneousAbort` mis-mapped to fatal). Pure
/// launch ordering; no failover logic.
const BOOT_STAGGER: Duration = Duration::from_secs(3);
/// Deadline for the cluster to fully boot.
const CLUSTER_BOOT_DEADLINE: Duration = Duration::from_secs(40);
/// Deadline for the post-kill failover to complete the workflow.
const FAILOVER_DEADLINE: Duration = Duration::from_secs(90);
/// Deadline for a `start` to land on node 0's shard (it is retried until it does).
const START_DEADLINE: Duration = Duration::from_secs(30);
/// Per-activity delay forced into the worker so the fan-out stays in flight long
/// enough for the kill to interrupt it (4 ordinals served serially through the
/// single push serve loop, so the total in-flight window is ~4x this).
const FAN_ACTIVITY_DELAY_MS: &str = "1500";

/// A booted node: its child process, ports, and log path.
struct Node {
    child: Child,
    http_port: u16,
    grpc_port: u16,
    liminal_port: u16,
    log_path: PathBuf,
}

/// The node index that is killed (it always owns shard 0 and hosts the worker's
/// first connection).
const OWNER_INDEX: usize = 0;
/// The node index designated as the SOLE adopter of the killed owner's shard.
/// Pinning the adopter (rather than letting both survivors race the shard
/// election) makes the single worker's redial deterministic: it migrates to its
/// next candidate, which is this node, and lands on the genuine new owner. Every
/// node still lists every peer for the distribution mesh + quorum; only the
/// killed owner's `owned_shards` is declared exclusively here.
const ADOPTER_INDEX: usize = 1;

/// Emit one node's TOML config: an N-node haematite cluster with the liminal
/// outbox transport. Node `index` owns shard `index`. The killed owner
/// ([`OWNER_INDEX`]) is declared as an adoptable peer ONLY in the designated
/// adopter's config ([`ADOPTER_INDEX`]); every other node lists it as a peer for
/// the mesh/quorum but with no owned shards, so exactly one survivor adopts it.
fn write_node_config(
    dir: &Path,
    index: usize,
    package: &Path,
    ports: &[(u16, u16, u16)],
) -> Result<PathBuf, TestError> {
    let (http_port, grpc_port, liminal_port) = ports[index];
    let data_dir = dir.join(format!("data{index}"));
    let bind_port = 7000 + index;

    let mut members = String::new();
    for node in 0..ports.len() {
        let _ = write!(members, "\"node-{node}@127.0.0.1\", ");
    }
    let members = members.trim_end_matches(", ");

    let mut peers = String::new();
    for (peer, &(_, peer_grpc, _)) in ports.iter().enumerate() {
        if peer == index {
            continue;
        }
        // The killed owner's shard is adopted ONLY by the designated adopter, so
        // the redial lands deterministically on the genuine new owner. Other
        // peers keep their own shard as their declared adoptable set.
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
            peer_bind = 7000 + peer,
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
transport = "liminal"
liminal_listen_address = "127.0.0.1:{liminal_port}"
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
fn boot_node(config: &Path, log_path: &Path, ports: (u16, u16, u16)) -> Result<Node, TestError> {
    let log = std::fs::File::create(log_path)?;
    let stderr = log.try_clone()?;
    let child = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(["server", "--config", &config.to_string_lossy()])
        .env(
            "AION_HOME",
            std::env::temp_dir().join(format!("aion-e2e-home-{}", std::process::id())),
        )
        .env("RUST_LOG", "info")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()?;
    Ok(Node {
        child,
        http_port: ports.0,
        grpc_port: ports.1,
        liminal_port: ports.2,
        log_path: log_path.to_path_buf(),
    })
}

#[test]
#[ignore = "slow: spawns a 3-node aion-server cluster + a liminal worker, kill -9 failover"]
fn osproc_kill9_liminal_failover_completes_exactly_once() -> Result<(), TestError> {
    let temp = tempfile::tempdir()?;
    let package = write_package_archive(temp.path())?;
    let worker_binary = build_worker_binary()?;

    // Reserve every port up front so configs can cross-reference peers' grpc.
    let mut ports: Vec<(u16, u16, u16)> = Vec::with_capacity(NODE_COUNT);
    for _ in 0..NODE_COUNT {
        ports.push((reserve_port()?, reserve_port()?, reserve_port()?));
    }

    let mut nodes = boot_cluster(temp.path(), &package, &ports)?;

    let worker = match spawn_worker(temp.path(), &worker_binary, &nodes) {
        Ok(worker) => worker,
        Err(error) => {
            reap_all(nodes);
            return Err(error);
        }
    };

    let result = drive_failover(&mut nodes, &ports);
    if result.is_err() {
        dump_logs_if_requested(temp.path());
    }
    reap(worker);
    reap_all(nodes);
    result
}

/// On failure, copy the whole work dir (node logs + worker log + configs) to the
/// path in `KILL9_KEEP_LOGS`, for post-mortem diagnosis. No-op when unset.
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

/// Boot the N-node cluster, wait for every node's liveness probe, and confirm
/// each survivor commissioned its cluster supervisor (the auto-adoption driver).
/// Reaps any already-booted nodes on failure so a partial boot never leaks.
fn boot_cluster(
    dir: &Path,
    package: &Path,
    ports: &[(u16, u16, u16)],
) -> Result<Vec<Node>, TestError> {
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

/// Spawn ONE worker with ALL nodes' liminal addresses as candidates and wait for
/// its first registration (the readiness file). It dials node 0 first; on a drop
/// it migrates to the next candidate, re-registering there.
fn spawn_worker(dir: &Path, worker_binary: &Path, nodes: &[Node]) -> Result<Child, TestError> {
    let ready_file = dir.join("worker.ready");
    let mut worker_args: Vec<String> = Vec::new();
    for node in nodes {
        worker_args.push("--address".to_owned());
        worker_args.push(format!("127.0.0.1:{}", node.liminal_port));
    }
    worker_args.push("--identity".to_owned());
    worker_args.push("kill9-failover-worker".to_owned());
    worker_args.push("--ready-file".to_owned());
    worker_args.push(ready_file.to_string_lossy().into_owned());
    let worker_log = std::fs::File::create(dir.join("worker.log"))?;
    let worker_log_err = worker_log.try_clone()?;
    let worker = Command::new(worker_binary)
        .args(&worker_args)
        .env("RUST_LOG", "info")
        // Slow each fan-out activity so the fan-out cannot finish before the
        // owner is killed — the survivor must re-dispatch the pending ordinals to
        // the redialed worker. Without this the worker might serve all 4 before
        // the kill and the failover would not be genuinely exercised.
        .env("LIMINAL_FAN_DELAY_MS", FAN_ACTIVITY_DELAY_MS)
        .stdout(Stdio::from(worker_log))
        .stderr(Stdio::from(worker_log_err))
        .spawn()?;
    if wait_until(FAILOVER_DEADLINE, || ready_file.exists()) {
        Ok(worker)
    } else {
        reap(worker);
        Err("worker never registered against node 0's liminal listener".into())
    }
}

/// Drive the in-flight start, the kill-9, and the survivor-side exactly-once
/// assertion. `nodes[0]` is the owner that gets killed; the remainder survive.
fn drive_failover(nodes: &mut Vec<Node>, ports: &[(u16, u16, u16)]) -> Result<(), TestError> {
    // Start collect_four so it lands on shard 0 (node 0's shard). An unsteered
    // start to node 0 lands on a locally-owned shard or is fenced; retry until it
    // lands (mirrors mp-failover-spike.sh — there is no cross-shard start routing).
    let node0_grpc = format!("http://127.0.0.1:{}", ports[0].1);
    let mut workflow_id = String::new();
    let landed = wait_until(START_DEADLINE, || match try_start(&node0_grpc) {
        Some(id) => {
            workflow_id = id;
            true
        }
        None => false,
    });
    if !landed {
        return Err("collect_four never landed on node 0's shard".into());
    }

    // Confirm the fan-out is genuinely in flight (Running, NOT yet Completed)
    // before the kill, so the kill interrupts a real in-flight dispatch — the
    // per-activity delay keeps the fan-out from finishing first. If it raced to
    // Completed before we could observe Running, the failover was not exercised,
    // so that is a test failure, not a silent pass.
    let mut last_status = String::new();
    let in_flight = wait_until(FAILOVER_DEADLINE, || {
        let description = describe(&node0_grpc, &workflow_id);
        match description.as_ref().and_then(status_of) {
            Some(status) => {
                status.clone_into(&mut last_status);
                status == "Running"
            }
            None => false,
        }
    });
    if !in_flight {
        return Err(format!(
            "collect_four never observed Running on node 0 before the kill \
             (last status: {last_status:?}); the kill would not have interrupted an \
             in-flight dispatch"
        )
        .into());
    }

    // kill -9 node 0: the owner of shard 0 and the worker's connected server.
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

    // THE PROOF: read over a SURVIVOR's gRPC. The fan-out must reach Completed
    // (the worker redialed a survivor, re-registered, and the survivor
    // re-dispatched the remaining ordinals) with EXACTLY ONE ActivityCompleted
    // terminal per ordinal — exactly-once across the kill.
    let survivor_grpc: Vec<String> = nodes
        .iter()
        .map(|node| format!("http://127.0.0.1:{}", node.grpc_port))
        .collect();
    let mut final_description: Option<Value> = None;
    let completed = wait_until(FAILOVER_DEADLINE, || {
        for endpoint in &survivor_grpc {
            if let Some(description) = describe(endpoint, &workflow_id) {
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
        Err("fan-out did not reach Completed on any survivor after the kill-9 failover".into())
    }
}

/// Assert the completed run recorded EXACTLY ONE terminal per ordinal.
fn check_exactly_once(description: Option<&Value>) -> Result<(), TestError> {
    let description =
        description.ok_or_else(|| TestError::from("missing completed describe payload"))?;
    let completions = activity_completed_count(description);
    if completions == FAN_OUT {
        Ok(())
    } else {
        Err(format!(
            "exactly-once violated: expected {FAN_OUT} ActivityCompleted terminals \
             (one per ordinal), found {completions}"
        )
        .into())
    }
}

/// Reap every node so a failed assertion never leaks a cluster.
fn reap_all(nodes: Vec<Node>) {
    for node in nodes {
        reap(node.child);
    }
}
