#!/usr/bin/env bash
#
# demo-failover.sh — ONE-COMMAND launcher for the Aion failover demo.
#
# Boots an N-node (default 3) haematite+liminal Aion cluster on one laptop, runs
# a legible "AI-agent-task fleet" fan-out workload with a provable exactly-once
# result, and leaves it RUNNING so you can watch progress and then violently
# kill the work-owning node with scripts/demo/kill-owner.sh.
#
# It is idempotent: each run kills any stale `aion server`/worker, wipes the
# state dir, regenerates configs, and boots fresh. A teardown trap stops every
# process it spawned UNLESS --keep is passed (so the kill demo can run after).
#
# Usage:
#   scripts/demo/demo-failover.sh [--nodes N] [--keep] [--auto-kill]
#
#   (default)    boot + start workload, leave running, print how to watch/kill,
#                and EXIT leaving the cluster up (implies --keep).
#   --auto-kill  boot, start workload, then run kill-owner.sh inline and tear
#                down — the full rehearsable end-to-end (used by CI / a quick
#                self-check). Exits non-zero if exactly-once is not proven.
#   --nodes N    cluster size (default 3; <3 cannot survive a kill).
#
# Env overrides: DEMO_STATE_DIR, DEMO_BOOT_STAGGER, the DEMO_*_BASE ports.

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/demo/lib.sh
source "$ROOT/scripts/demo/lib.sh"

NODES=3
KEEP=0
AUTO_KILL=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --nodes)     NODES="${2:-}"; shift 2 ;;
    --keep)      KEEP=1; shift ;;
    --auto-kill) AUTO_KILL=1; shift ;;
    -h|--help)   sed -n '2,30p' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ "$NODES" -ge 1 ] || die "--nodes must be >= 1"
[ "$NODES" -ge 3 ] || note "WARNING: <3 nodes cannot survive a kill (quorum needs >=3)"
# Default behaviour leaves the cluster up so the kill demo can follow.
[ "$AUTO_KILL" = 0 ] && KEEP=1

STATE_DIR="${DEMO_STATE_DIR:-/tmp/aion-failover-demo}"
DEMO_BOOT_STAGGER="${DEMO_BOOT_STAGGER:-3}"
AION="$ROOT/target/debug/aion"
# DEMO_WORKER selects the fan-out workload's worker:
#   synthetic (default) — spike/liminal-fan-worker: returns a canned per-ordinal
#                         string after an artificial delay. Deterministic; the
#                         proven exactly-once failover gate.
#   norn               — examples/norn-fan-worker: each fan:N runs a REAL Norn AI
#                         agent step via the ChatGPT login. Same transport/redial,
#                         same exactly-once guarantee, but the fan-out is real AI.
DEMO_WORKER="${DEMO_WORKER:-synthetic}"
if [ "$DEMO_WORKER" = norn ]; then
  WORKER_DIR="$ROOT/examples/norn-fan-worker"
  WORKER_BIN="$WORKER_DIR/target/debug/norn-fan-worker"
  WORKER_NAME="norn-fan-worker"
  # Default to the sibling norn checkout's release binary; override with NORN_BIN.
  NORN_BIN="${NORN_BIN:-$ROOT/../norn/target/release/norn}"
else
  WORKER_DIR="$ROOT/spike/liminal-fan-worker"
  WORKER_BIN="$WORKER_DIR/target/debug/liminal-fan-worker"
  WORKER_NAME="liminal-fan-worker"
fi
PACKAGER_DIR="$ROOT/demo/fleet-packager"
PACKAGER_BIN="$PACKAGER_DIR/target/debug/fleet-packager"
PACKAGE="$STATE_DIR/fleet.aion"
FAN_OUT=4   # collect_four fans four agent-ish tasks; exactly-once total = 4
# Per-activity delay so the synthetic fan-out stays in flight across the kill (4
# ordinals served serially through the single push loop => ~4x this in-flight
# window). The norn worker needs no artificial delay: a real agent step's natural
# latency is the in-flight window.
export LIMINAL_FAN_DELAY_MS="${LIMINAL_FAN_DELAY_MS:-1500}"

NODE_PIDS=()
WORKER_PID=""
teardown() {
  local rc=$?   # preserve the script's exit status across the trap body
  if [ "$KEEP" = 1 ]; then return "$rc"; fi
  say "teardown"
  for pid in "${NODE_PIDS[@]:-}"; do [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null || true; done
  [ -n "$WORKER_PID" ] && kill -9 "$WORKER_PID" 2>/dev/null || true
  pkill -9 -f "$AION server" 2>/dev/null || true
  pkill -9 -f "fan-worker" 2>/dev/null || true   # matches liminal- and norn-fan-worker
  note "stopped all demo processes"
  return "$rc"
}
trap teardown EXIT

# ----------------------------------------------------------------------------
say "build (ops-console bundle + aion cluster binary + worker + packager)"
# Build the embedded console FIRST, with the demo's namespace grant baked in so the
# console's REST/WS calls carry x-aion-namespaces (without it the server replies
# namespace_denied and the UI shows "namespaces unavailable"). No deploy grant is
# baked: this demo runs the cluster with auth off, so the server is in single-tenant
# operator mode and grants the deploy-scoped WS3 cluster (failover topology) socket
# server-side at the handshake — the console discovers its deploy capability at
# runtime via GET /whoami. API/WS base is left unset = same origin, so each node
# serves a console that talks to ITSELF (correct for the failover own-read view).
# Optional: refresh the committed ops-console-embed/ bundle with this demo's baked
# VITE config. The ops console is ALWAYS embedded (the committed bundle ships in
# every build), so if bun is absent the build below still serves the real console
# from the committed bundle — just without these demo-specific VITE defaults.
( cd "$ROOT" && VITE_AION_NAMESPACES=default VITE_AION_SUBJECT=operator cargo xtask build-ops-console ) \
  || note "ops-console bundle rebuild skipped/failed (need bun) — nodes serve the committed bundle"
# The ops console is always embedded (rust_embed over the committed ops-console-embed/),
# so each node serves the real ops console at its HTTP port with no extra feature.
( cd "$ROOT" && cargo build -p aion-cli --features haematite-backend,liminal-transport ) \
  || die "aion CLI build failed"
( cd "$WORKER_DIR" && cargo build ) || die "$WORKER_NAME build failed"
( cd "$PACKAGER_DIR" && cargo build ) || die "fleet-packager build failed"
ok "built aion (+haematite-backend,+liminal-transport,+embedded ops console), $WORKER_NAME, packager"

# Real-AI worker preflight: the Norn binary must exist and be logged in (the
# project uses the ChatGPT OAuth login, not an API key), or every fan:N step
# fails. Fail fast here with a clear message rather than mid-demo.
if [ "$DEMO_WORKER" = norn ]; then
  [ -x "$NORN_BIN" ] || die "norn binary not found/executable at $NORN_BIN (build it or set NORN_BIN)"
  env -u OPENAI_API_KEY "$NORN_BIN" auth status >/dev/null 2>&1 \
    || die "norn is not logged in — run '$NORN_BIN auth login' (ChatGPT login) first"
  ok "norn ready: $NORN_BIN (ChatGPT login active)"
fi

say "clean stale processes + state"
pkill -9 -f "$AION server" 2>/dev/null || true
pkill -9 -f "fan-worker" 2>/dev/null || true   # matches liminal- and norn-fan-worker
sleep 1
rm -rf "$STATE_DIR"
mkdir -p "$STATE_DIR/logs"
ok "killed any stale aion server/worker; reset $STATE_DIR"

say "package the fleet workload"
"$PACKAGER_BIN" "$PACKAGE" >/dev/null || die "packaging the fleet workload failed"
ok "fleet workload packaged -> $PACKAGE (collect_four: $FAN_OUT agent tasks)"

say "generate $NODES-node cluster config (single-laptop)"
emit_cluster_config "$STATE_DIR" "$NODES" "$PACKAGE" "127.0.0.1" "loopback"
for ((i=0; i<NODES; i++)); do
  note "node$i: http=$(http_port "$i") grpc=$(grpc_port "$i") bind=$(bind_port "$i") liminal=$(liminal_port "$i") owns shard $i"
done
ok "configs written to $STATE_DIR/node*.toml"

say "boot $NODES nodes (staggered ${DEMO_BOOT_STAGGER}s to dodge the boot race)"
for ((i=0; i<NODES; i++)); do
  RUST_LOG=info "$AION" server --config "$STATE_DIR/node$i.toml" \
    > "$STATE_DIR/logs/node$i.log" 2>&1 &
  NODE_PIDS[$i]=$!
  note "node$i pid=${NODE_PIDS[$i]}"
  [ "$i" -lt "$((NODES-1))" ] && sleep "$DEMO_BOOT_STAGGER"
done

for ((i=0; i<NODES; i++)); do
  wait_for_live "127.0.0.1" "$(http_port "$i")" 40 \
    || die "node$i did not answer /health/live (see $STATE_DIR/logs/node$i.log)"
done
ok "all $NODES nodes are live (HTTP /health/live = 200)"
for ((i=0; i<NODES; i++)); do
  wait_for_file_line "$STATE_DIR/logs/node$i.log" "commissioned" 30 \
    || die "node$i did not commission its cluster supervisor"
done
ok "every node commissioned its SS-5b cluster supervisor (failover driver)"

say "start the fleet worker (redial-capable across all $NODES nodes)"
WORKER_ARGS=()
for ((i=0; i<NODES; i++)); do
  WORKER_ARGS+=("--address" "127.0.0.1:$(liminal_port "$i")")
done
WORKER_ARGS+=("--identity" "fleet-worker" "--ready-file" "$STATE_DIR/worker.ready")
[ "$DEMO_WORKER" = norn ] && WORKER_ARGS+=("--norn-bin" "$NORN_BIN")
RUST_LOG=info "$WORKER_BIN" "${WORKER_ARGS[@]}" > "$STATE_DIR/logs/worker.log" 2>&1 &
WORKER_PID=$!
if [ "$DEMO_WORKER" = norn ]; then
  note "worker pid=$WORKER_PID ($WORKER_NAME — each fan:N is a REAL Norn agent step)"
else
  note "worker pid=$WORKER_PID ($WORKER_NAME — per-task delay ${LIMINAL_FAN_DELAY_MS}ms so progress is visible)"
fi
i=0; while [ "$i" -lt 120 ] && [ ! -f "$STATE_DIR/worker.ready" ]; do sleep 0.25; i=$((i+1)); done
[ -f "$STATE_DIR/worker.ready" ] || die "worker never registered against a node's liminal listener"
ok "worker connected + registered (ready file present)"

say "start the fleet workload on the owner node (shard 0)"
# Unsteered start to the owner lands on a locally-owned shard or is fenced;
# retry until it lands on node 0's shard (no cross-shard start routing yet).
OWNER_GRPC="http://127.0.0.1:$(grpc_port "$DEMO_OWNER_INDEX")"
WORKLOAD_ID=""
for attempt in $(seq 1 30); do
  OUT="$("$AION" start aion_outbox_fixture --input '{"fleet":"demo"}' \
        --endpoint "$OWNER_GRPC" 2>&1)" || true
  WORKLOAD_ID="$(printf '%s' "$OUT" | sed -n 's/.*"workflow_id":"\([^"]*\)".*/\1/p')"
  [ -n "$WORKLOAD_ID" ] && { note "started on attempt $attempt -> $WORKLOAD_ID"; break; }
  dim "attempt $attempt fenced (wrong shard for owner), retrying"
done
[ -n "$WORKLOAD_ID" ] || die "could not land the fleet workload on the owner's shard"
ok "fleet workload running on node $DEMO_OWNER_INDEX: $WORKLOAD_ID"

# Persist run state so kill-owner.sh can act + assert.
{
  echo "AION=$AION"
  echo "NODES=$NODES"
  echo "FAN_OUT=$FAN_OUT"
  echo "WORKLOAD_ID=$WORKLOAD_ID"
  echo "OWNER_PID=${NODE_PIDS[$DEMO_OWNER_INDEX]}"
  echo "DEMO_OWNER_INDEX=$DEMO_OWNER_INDEX"
  echo "DEMO_ADOPTER_INDEX=$DEMO_ADOPTER_INDEX"
  echo "DEMO_GRPC_BASE=$DEMO_GRPC_BASE"
  echo "DEMO_HTTP_BASE=$DEMO_HTTP_BASE"
} > "$STATE_DIR/demo.state"

if [ "$AUTO_KILL" = 1 ]; then
  say "auto-kill: running the violent kill + exactly-once assertion inline"
  KEEP=1  # keep nodes up for kill-owner; we tear down explicitly after
  DEMO_STATE_DIR="$STATE_DIR" "$ROOT/scripts/demo/kill-owner.sh" --state-dir "$STATE_DIR"
  rc=$?
  KEEP=0  # let the trap tear the survivors down
  exit "$rc"
fi

# Default path: leave it running and tell the operator how to watch + kill.
say "CLUSTER UP — workload running. The cluster is LEFT RUNNING."
note ""
note "WATCH progress (per-ordinal terminals climb 0 -> $FAN_OUT over ~tens of seconds):"
note "  watch -n1 '$AION describe $WORKLOAD_ID --endpoint $OWNER_GRPC | grep -c ActivityCompleted'"
note ""
note "STATUS surfaces an ops console can poll (see docs/demo/FAILOVER-DEMO.md):"
note "  node liveness : curl -s http://127.0.0.1:$(http_port 0)/health/live"
note "  metrics       : curl -s http://127.0.0.1:$(http_port 0)/metrics"
note "  workflow list : $AION list --endpoint $OWNER_GRPC"
note "  run detail    : $AION describe $WORKLOAD_ID --endpoint $OWNER_GRPC"
note "  live events   : ws://127.0.0.1:$(http_port 0)/events/stream"
note ""
note "VIOLENTLY KILL the work-owning node + assert EXACTLY-ONCE on a survivor:"
note "  scripts/demo/kill-owner.sh --state-dir $STATE_DIR"
note ""
note "TEARDOWN when done:"
note "  pkill -9 -f '$AION server'; pkill -9 -f fan-worker"
note ""
note "(logs: $STATE_DIR/logs/)"
