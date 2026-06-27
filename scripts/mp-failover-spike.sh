#!/usr/bin/env bash
#
# mp-failover-spike.sh — TRUE multi-OS-process aion-on-haematite cluster spike.
#
# Stands up real `aion server` OS processes on the haematite backend with a
# `[store.cluster]` section, real out-of-process activity workers, and a real
# workflow, and exercises cross-process dispatch / replication / failover.
#
# WHAT THIS DEMONSTRATES (verified 2026-06-27, branch mp-cluster-spike):
#   PHASE A (2-node cluster)  — WORKS end to end:
#     1. two aion server processes boot, elect their shards, connect, and each
#        commission the SS-5b cluster supervisor;
#     2. a workflow submitted over gRPC COMPLETES, its `greet` activity run by a
#        separate worker OS process (cross-process dispatch + result flow);
#     3. the committed history is REPLICATED — the same run is fully visible via
#        the PEER node's gRPC endpoint.
#   PHASE B (3-node cluster, required for real failover) — BLOCKED at boot:
#     the beamr/haematite distribution handshake (DistributionEndpoint::connect)
#     is a blocking, timeout-less, mutual exchange that DEADLOCKS when 3 peers
#     form a mesh across separate processes. Nodes 1 and 2 hang permanently in
#     `connect`. kill -9 failover therefore cannot be reached today.
#
# WHY 3 NODES: haematite quorum_size(2) = 2, so a 2-node cluster losing one node
# leaves a single survivor that can NEVER form a majority — adopt_shards (which
# re-elects) would fail. Cross-process failover needs >=3 nodes (quorum_size(3)=2),
# which is exactly where the handshake deadlock bites.
#
# Re-runnable; tears down its own processes and data on each run.
#
# Usage: scripts/mp-failover-spike.sh
set -uo pipefail

# ----------------------------------------------------------------------------
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${MP_SPIKE_DIR:-/tmp/mp-failover-spike}"
AION="$ROOT/target/debug/aion"
WORKER="$ROOT/spike/greet-worker/target/debug/greet-worker"
HELLO_SRC="$ROOT/examples/hello-world"
ARCHIVE="$WORK/hello-world.aion"

say()  { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
note() { printf '   %s\n' "$*"; }
ok()   { printf '   \033[1;32mOK\033[0m  %s\n' "$*"; }
bad()  { printf '   \033[1;31mXX\033[0m  %s\n' "$*"; }

PIDS=()
cleanup() {
  say "teardown"
  pkill -9 -f "$AION server" 2>/dev/null
  pkill -9 -f "greet-worker" 2>/dev/null
  sleep 1
  note "stopped all spike processes"
}
trap cleanup EXIT

wait_for() { # <file> <pattern> <timeout-s>
  local f="$1" pat="$2" t="$3" i=0
  while [ "$i" -lt "$((t*4))" ]; do
    grep -q "$pat" "$f" 2>/dev/null && return 0
    sleep 0.25; i=$((i+1))
  done
  return 1
}

# ----------------------------------------------------------------------------
say "build"
( cd "$ROOT" && cargo build -p aion-cli --features haematite-backend ) || { bad "CLI build failed"; exit 1; }
( cd "$ROOT/spike/greet-worker" && cargo build ) || { bad "worker build failed"; exit 1; }
ok "aion CLI (+haematite-backend) and greet-worker built"

say "workspace"
rm -rf "$WORK"; mkdir -p "$WORK/logs"
( cd "$HELLO_SRC" && gleam build >/dev/null 2>&1 )
"$AION" package "$HELLO_SRC" --out "$ARCHIVE" >/dev/null 2>&1 || { bad "package failed"; exit 1; }
ok "hello-world packaged -> $ARCHIVE"

# ----------------------------------------------------------------------------
# Emit an N-node config set. node i owns shard i; full mesh peers.
emit_configs() { # <count>
  local n="$1" i j
  for ((i=0;i<n;i++)); do
    local grpc=$((50051+i)) http=$((8080+i)) bind=$((7000+i))
    local members="" peers=""
    for ((j=0;j<n;j++)); do members="$members\"mp-node-$j@127.0.0.1\", "; done
    for ((j=0;j<n;j++)); do
      [ "$j" -eq "$i" ] && continue
      peers="$peers
[[store.cluster.peers]]
name = \"mp-node-$j@127.0.0.1\"
address = \"127.0.0.1:$((7000+j))\"
owned_shards = [$j]
"
    done
    cat > "$WORK/n$i.toml" <<TOML
workflow_packages = ["$ARCHIVE"]

[server]
listen_address = "127.0.0.1:$http"
grpc_address = "127.0.0.1:$grpc"

[store]
backend = "haematite"
data_dir = "$WORK/d$i"
shard_count = $n
owned_shards = [$i]

[store.cluster]
node_id = "mp-node-$i@127.0.0.1"
bind_address = "127.0.0.1:$bind"
members = [${members%, }]
failover_poll_interval_ms = 500
failover_confirmations = 3
$peers
[runtime]
query_timeout_ms = 10000

[namespaces]
default = "default"

[websocket]
event_broadcast_capacity = 1024
TOML
  done
}

boot_node() { # <i>
  local i="$1"
  RUST_LOG=info "$AION" server --config "$WORK/n$i.toml" > "$WORK/logs/n$i.log" 2>&1 &
  PIDS[$i]=$!
  note "node$i pid=${PIDS[$i]} (grpc 5005$((1+i)), bind 700$i, owns shard $i)"
}

# ============================================================================
say "PHASE A — 2-node cluster (cross-process dispatch + replication)"
emit_configs 2
rm -rf "$WORK/d0" "$WORK/d1"
boot_node 0
boot_node 1
if wait_for "$WORK/logs/n0.log" "startup banner" 20 && wait_for "$WORK/logs/n1.log" "startup banner" 20; then
  ok "both nodes booted + elected shards"
  grep -q commissioned "$WORK/logs/n0.log" && grep -q commissioned "$WORK/logs/n1.log" \
    && ok "both nodes commissioned the SS-5b cluster supervisor"
else
  bad "2-node cluster failed to boot (see $WORK/logs)"; exit 1
fi

say "PHASE A — workers (one per node, separate OS processes)"
RUST_LOG=info "$WORKER" --endpoint http://127.0.0.1:50051 --identity worker-0 > "$WORK/logs/w0.log" 2>&1 &
RUST_LOG=info "$WORKER" --endpoint http://127.0.0.1:50052 --identity worker-1 > "$WORK/logs/w1.log" 2>&1 &
wait_for "$WORK/logs/w0.log" "session established" 10 && wait_for "$WORK/logs/w1.log" "session established" 10 \
  && ok "both workers connected" || { bad "workers failed to connect"; exit 1; }

say "PHASE A — submit a workflow, prove it COMPLETES cross-process"
sleep 2  # let both worker sessions settle into the poll loop
# KNOWN LIMITATION (finding #3): a `start` issued to a node whose freshly-
# generated workflow_id hashes to a shard the node does NOT own is FENCED
# ("fenced by CAS rejects") — there is no shard-owner request routing yet, so
# ~half of submissions to either node fail at random. We retry until one lands
# on the receiving node's own shard, which is the path that genuinely works.
WID=""
for attempt in $(seq 1 20); do
  OUT=$("$AION" start hello_world --input '{"name":"Cluster"}' --endpoint http://127.0.0.1:50051 2>&1)
  WID=$(printf '%s' "$OUT" | sed -n 's/.*"workflow_id":"\([^"]*\)".*/\1/p')
  if [ -n "$WID" ]; then note "start succeeded on attempt $attempt -> $WID"; break; fi
  printf '   .. attempt %s fenced (wrong shard for node 0), retrying\n' "$attempt"
done
[ -n "$WID" ] || { bad "every start was fenced — could not land a workflow on node 0's shard"; exit 1; }
# Poll describe until terminal (the workflow may run on EITHER node's shard;
# describe reads from any node, so query node 0 and let replication serve it).
# Extract summary.status from the describe JSON with sed (no python dependency).
describe_status() { # <endpoint>
  "$AION" describe "$WID" --endpoint "$1" 2>/dev/null \
    | tr -d '\n' | sed -n 's/.*"summary":{[^}]*"status":"\([^"]*\)".*/\1/p'
}
STATUS=""
for _ in $(seq 1 30); do
  STATUS=$(describe_status http://127.0.0.1:50051)
  case "$STATUS" in Completed|Failed|Cancelled|TimedOut) break;; esac
  sleep 0.5
done
[ "$STATUS" = "Completed" ] && ok "workflow COMPLETED on node 0 (status=$STATUS)" \
  || { bad "workflow did not complete (status=$STATUS)"; exit 1; }

say "PHASE A — replication check (same run, via the PEER node's endpoint)"
PEER_STATUS=$(describe_status http://127.0.0.1:50052)
PEER_EVENTS=$("$AION" describe "$WID" --endpoint http://127.0.0.1:50052 2>/dev/null \
  | grep -o '"type":"' | wc -l | tr -d ' ')
if [ "$PEER_STATUS" = "Completed" ]; then
  ok "peer node sees the SAME committed history (status=$PEER_STATUS, events=$PEER_EVENTS)"
  ok "PHASE A PASSED: cross-process dispatch, completion, and replication all work"
else
  bad "peer node does NOT have the run replicated (status='$PEER_STATUS')"; exit 1
fi

# ============================================================================
say "PHASE B — 3-node cluster (REQUIRED for real kill -9 failover)"
note "quorum_size(2)=2 => a 2-node survivor can never re-elect; failover needs >=3 nodes."
pkill -9 -f "$AION server" 2>/dev/null; pkill -9 -f greet-worker 2>/dev/null; sleep 1
emit_configs 3
rm -rf "$WORK/d0" "$WORK/d1" "$WORK/d2"
boot_node 0; boot_node 1; boot_node 2
note "waiting up to 30s for all three to boot..."
A=0; B=0; C=0
if wait_for "$WORK/logs/n0.log" "startup banner" 30; then A=1; fi
if wait_for "$WORK/logs/n1.log" "startup banner" 5;  then B=1; fi
if wait_for "$WORK/logs/n2.log" "startup banner" 5;  then C=1; fi
note "booted: node0=$A node1=$B node2=$C"
if [ "$A" = 1 ] && [ "$B" = 1 ] && [ "$C" = 1 ]; then
  ok "3-node cluster booted — proceed to kill -9 failover (TODO: not reached in this run)"
  # If we ever get here, the deadlock is fixed; the kill -9 step would go here.
else
  bad "PHASE B BLOCKED: 3-node cross-process boot deadlocked."
  note "Root cause: beamr DistributionEndpoint::connect is a blocking, timeout-less"
  note "mutual handshake; a 3-peer mesh across processes deadlocks. Nodes hang in"
  note "haematite::sync::endpoint::DistributionEndpoint::connect (verify with"
  note "\`sample <pid>\`). This is a beamr-layer fix (handshake read timeout or"
  note "simultaneous-connect tie-break), out of scope for this aion-side spike."
  note "kill -9 failover cannot be demonstrated end-to-end until it is fixed."
fi

say "DONE — logs in $WORK/logs"
