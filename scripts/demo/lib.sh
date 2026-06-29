#!/usr/bin/env bash
#
# lib.sh — shared helpers for the Aion failover demo scripts.
#
# Sourced by demo-failover.sh, gen-cluster-config.sh, and kill-owner.sh. Holds
# the cluster-config GENERATOR, port maths, colour helpers, polling, and the
# `aion` client wrappers. All failover intelligence lives in shipped library
# code (the SS-5b ClusterSupervisor + Engine::adopt_shards + #73 routing + the
# worker's serve_with_redial); these scripts only boot processes, drive the CLI,
# kill -9, and poll real observables.
#
# Conventions used throughout the demo, all sidestepping known footguns:
#   * ports live in the 7700+ (replication bind), 50051+ (gRPC), 8090+ (HTTP),
#     8190+ (liminal) ranges. TCP 7000 is AVOIDED — macOS AirPlay Receiver
#     squats on it. The old spikes used 7000/8080; the demo does not.
#   * boots are staggered (DEMO_BOOT_STAGGER, default 3s) to sidestep the beamr
#     simultaneous-connect boot race (a benign SimultaneousAbort mis-mapped to a
#     fatal error). Pure launch ordering — no failover logic.
#   * the killed owner (node 0) is adopted by exactly ONE survivor (node 1) so a
#     single redial-capable worker migrates deterministically to the new owner.

set -euo pipefail

# ----------------------------------------------------------------------------
# Port base constants. Override via the environment for a busy host.
DEMO_BIND_BASE="${DEMO_BIND_BASE:-7700}"   # haematite replication bind
DEMO_GRPC_BASE="${DEMO_GRPC_BASE:-50051}"  # gRPC API + worker pull protocol
DEMO_HTTP_BASE="${DEMO_HTTP_BASE:-8090}"   # HTTP/JSON API + dashboard + health
DEMO_LIMINAL_BASE="${DEMO_LIMINAL_BASE:-8190}" # liminal push listener

# The node that is killed (always owns shard 0 and the worker's first link).
DEMO_OWNER_INDEX="${DEMO_OWNER_INDEX:-0}"
# The sole survivor declared as the killed owner's shard adopter, so the single
# worker's redial lands deterministically on the genuine new owner.
DEMO_ADOPTER_INDEX="${DEMO_ADOPTER_INDEX:-1}"

# ----------------------------------------------------------------------------
# Colour helpers (no-op when stdout is not a TTY).
if [ -t 1 ]; then
  _C_HEAD=$'\033[1;36m'; _C_OK=$'\033[1;32m'; _C_BAD=$'\033[1;31m'
  _C_DIM=$'\033[2m'; _C_RST=$'\033[0m'
else
  _C_HEAD=""; _C_OK=""; _C_BAD=""; _C_DIM=""; _C_RST=""
fi
say()  { printf '\n%s== %s ==%s\n' "$_C_HEAD" "$*" "$_C_RST"; }
note() { printf '   %s\n' "$*"; }
dim()  { printf '   %s%s%s\n' "$_C_DIM" "$*" "$_C_RST"; }
ok()   { printf '   %sOK%s  %s\n' "$_C_OK" "$_C_RST" "$*"; }
bad()  { printf '   %sXX%s  %s\n' "$_C_BAD" "$_C_RST" "$*" >&2; }
die()  { bad "$*"; exit 1; }

# ----------------------------------------------------------------------------
# Per-node port accessors (index in, port out).
bind_port()    { echo $(( DEMO_BIND_BASE + $1 )); }
grpc_port()    { echo $(( DEMO_GRPC_BASE + $1 )); }
http_port()    { echo $(( DEMO_HTTP_BASE + $1 )); }
liminal_port() { echo $(( DEMO_LIMINAL_BASE + $1 )); }

# ----------------------------------------------------------------------------
# emit_cluster_config — the GENERATOR.
#
# Writes one valid per-node aion.toml into <out-dir>/nodeN.toml for an N-node
# haematite+liminal cluster. node i owns shard i; every node lists every peer
# for the distribution mesh + quorum. The killed owner's shard is declared
# adoptable ONLY in the designated adopter's config, so exactly one survivor
# adopts it (deterministic single-worker redial).
#
# Args:
#   $1  out-dir            (configs + per-node data dirs live here)
#   $2  node-count         (>= 3 for quorum-backed failover)
#   $3  package-path       (the .aion workload archive, absolute)
#   $4  host               ("127.0.0.1" single-laptop, or a real/Tailscale IP)
#   $5  members-host-mode  ("loopback" => each node host is 127.0.0.1;
#                           "shared"   => every node uses $host, real-IP mode)
#
# In loopback mode each node binds 127.0.0.1 with distinct ports (single laptop).
# In shared mode each node binds $host (one IP per host); run one node per host
# with the SAME out-dir contents copied out, or distinct hosts via DEMO_HOSTS.
emit_cluster_config() {
  local out_dir="$1" count="$2" package="$3" host="$4" mode="$5"
  local i j
  [ "$count" -ge 1 ] || die "node-count must be >= 1 (got $count)"
  mkdir -p "$out_dir"

  # Resolve a per-node host: shared mode uses $host for every node; loopback
  # mode uses 127.0.0.1. DEMO_HOSTS (space-separated) overrides per node index
  # for a true multi-host deploy.
  node_host() {
    local idx="$1"
    if [ -n "${DEMO_HOSTS:-}" ]; then
      local arr; read -r -a arr <<< "$DEMO_HOSTS"
      [ "$idx" -lt "${#arr[@]}" ] || die "DEMO_HOSTS has fewer entries than nodes"
      printf '%s' "${arr[$idx]}"
    elif [ "$mode" = "shared" ]; then
      printf '%s' "$host"
    else
      printf '127.0.0.1'
    fi
  }

  for ((i=0; i<count; i++)); do
    local h_http h_grpc h_bind h_lim self_host cors
    self_host="$(node_host "$i")"
    h_http="$(http_port "$i")"; h_grpc="$(grpc_port "$i")"
    h_bind="$(bind_port "$i")"; h_lim="$(liminal_port "$i")"

    # CORS origins the browser dashboard is served from. Defaults to the local
    # Vite dev server (unchanged single-laptop demo). For a multi-host mesh where
    # the dashboard runs on a routable host, set DEMO_DASHBOARD_ORIGINS to a
    # space-separated origin list, e.g.
    #   DEMO_DASHBOARD_ORIGINS="http://100.0.0.9:5173 https://dash.example"
    if [ -n "${DEMO_DASHBOARD_ORIGINS:-}" ]; then
      local origin arr_o
      read -r -a arr_o <<< "$DEMO_DASHBOARD_ORIGINS"
      cors=""
      for origin in "${arr_o[@]}"; do
        cors+="\"$origin\", "
      done
      cors="${cors%, }"
    else
      cors="\"http://localhost:5173\", \"http://127.0.0.1:5173\""
    fi

    local members="" peers=""
    for ((j=0; j<count; j++)); do
      members+="\"node-$j@$(node_host "$j")\", "
    done
    members="${members%, }"

    for ((j=0; j<count; j++)); do
      [ "$j" -eq "$i" ] && continue
      local peer_host peer_bind peer_grpc owned
      peer_host="$(node_host "$j")"
      peer_bind="$(bind_port "$j")"; peer_grpc="$(grpc_port "$j")"
      # The killed owner's shard is adopted only by the designated adopter, so a
      # single worker's redial reaches the genuine new owner. Every other peer
      # keeps its own shard as its declared adoptable set.
      if [ "$j" -eq "$DEMO_OWNER_INDEX" ] && [ "$i" -ne "$DEMO_ADOPTER_INDEX" ]; then
        owned=""
      else
        owned="$j"
      fi
      peers+="
[[store.cluster.peers]]
name = \"node-$j@$peer_host\"
address = \"$peer_host:$peer_bind\"
grpc_address = \"$peer_host:$peer_grpc\"
owned_shards = [$owned]
"
    done

    cat > "$out_dir/node$i.toml" <<TOML
# Generated by scripts/demo/gen-cluster-config.sh — node $i of $count.
# Do not hand-edit; regenerate. Owns shard $i; full mesh peers; liminal push.
workflow_packages = ["$package"]

[server]
listen_address = "$self_host:$h_http"
grpc_address = "$self_host:$h_grpc"
# Allow the browser dashboard (served from the Vite dev server on a different
# origin) to call this node's HTTP API cross-origin. Secure-by-default: only
# these exact origins are permitted; unset means no cross-origin access.
# Override via DEMO_DASHBOARD_ORIGINS for a dashboard on a routable host.
cors_allowed_origins = [$cors]

[store]
backend = "haematite"
data_dir = "$out_dir/data$i"
shard_count = $count
owned_shards = [$i]

[store.cluster]
node_id = "node-$i@$self_host"
bind_address = "$self_host:$h_bind"
members = [$members]
failover_poll_interval_ms = 500
failover_confirmations = 3
$peers
[runtime]
query_timeout_ms = 10000

[namespaces]
default = "default"

[websocket]
event_broadcast_capacity = 1024
# WS3 cluster topology push channel (always mounted on /events/stream). Cluster
# events are low-rate, so a modest capacity is plenty.
cluster_broadcast_capacity = 256

[outbox]
enabled = true
poll_interval_ms = 20
batch_size = 16
max_attempts = 5
backoff_base_ms = 50
backoff_multiplier = 2
backoff_max_ms = 1000
transport = "liminal"
liminal_listen_address = "$self_host:$h_lim"

[deploy]
# Mount the operator deploy surface so the dashboard's "deploy package" action
# works against the demo. Dark-by-default in the server (absent section => the
# /deploy/* HTTP routes and the gRPC DeployService are not mounted), so the demo
# opts in explicitly here. Both ceilings are REQUIRED when enabled (no server
# defaults) and max_inflated_bytes must be >= max_archive_bytes. Override per
# node without regenerating via AION_DEPLOY_* env vars.
enabled = true
max_archive_bytes = 67108864
max_inflated_bytes = 268435456
TOML
  done
}

# ----------------------------------------------------------------------------
# aion client wrappers. AION must point at the built `aion` binary.
# describe_status <grpc-endpoint> <workflow-id> -> prints summary.status or empty
describe_status() {
  "$AION" describe "$2" --endpoint "$1" 2>/dev/null \
    | tr -d '\n' | sed -n 's/.*"summary":{[^}]*"status":"\([^"]*\)".*/\1/p'
}

# activity_completed_count <grpc-endpoint> <workflow-id> -> count of terminals
activity_completed_count() {
  "$AION" describe "$2" --endpoint "$1" 2>/dev/null \
    | grep -o '"type":"ActivityCompleted"' | wc -l | tr -d ' '
}

# wait_for_file_line <file> <pattern> <timeout-s> -> 0 if found
wait_for_file_line() {
  local f="$1" pat="$2" t="$3" i=0
  while [ "$i" -lt "$((t*4))" ]; do
    grep -q -- "$pat" "$f" 2>/dev/null && return 0
    sleep 0.25; i=$((i+1))
  done
  return 1
}

# http_live <host> <http-port> -> 0 if /health/live answers 200
http_live() {
  local body
  body="$(curl -fsS --max-time 2 "http://$1:$2/health/live" 2>/dev/null)" || return 1
  [ -n "$body" ] || true
  return 0
}

# wait_for_live <host> <http-port> <timeout-s> -> 0 when live
wait_for_live() {
  local host="$1" port="$2" t="$3" i=0
  while [ "$i" -lt "$((t*4))" ]; do
    http_live "$host" "$port" && return 0
    sleep 0.25; i=$((i+1))
  done
  return 1
}
