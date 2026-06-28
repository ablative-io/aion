#!/usr/bin/env bash
#
# kill-owner.sh — the VIOLENT kill control + EXACTLY-ONCE assertion.
#
# `kill -9`s the node that owns the running workload's shard (node 0, the owner
# of shard 0 and the worker's first liminal link), then ASSERTS — purely by
# polling a SURVIVOR's gRPC — that the fleet workflow reaches Completed with
# EXACTLY ONE ActivityCompleted terminal per fan-out ordinal (exactly-once
# across the kill). No sleeps are used as proof; every assertion reads a real
# observable (the SS-5b auto-adoption log line, then describe over gRPC).
#
# All failover intelligence lives in shipped library code; this script only
# kill -9's and polls. It reads the run state demo-failover.sh wrote.
#
# Usage:
#   scripts/demo/kill-owner.sh [--state-dir DIR]
# (defaults to the dir in DEMO_STATE_DIR, else /tmp/aion-failover-demo)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/demo/lib.sh
source "$ROOT/scripts/demo/lib.sh"

STATE_DIR="${DEMO_STATE_DIR:-/tmp/aion-failover-demo}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --state-dir) STATE_DIR="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

STATE="$STATE_DIR/demo.state"
[ -f "$STATE" ] || die "no demo state at $STATE — run scripts/demo/demo-failover.sh first"
# shellcheck source=/dev/null
source "$STATE"   # AION, NODES, WORKLOAD_ID, FAN_OUT, OWNER_PID, owner/survivor ports

[ -n "${WORKLOAD_ID:-}" ] || die "no WORKLOAD_ID in state — the workload never started"
[ -n "${OWNER_PID:-}" ] || die "no OWNER_PID in state"

ADOPT_LINE="adopted a downed peer's shards (SS-5b auto-failover)"

say "kill -9 the work-owning node (node $DEMO_OWNER_INDEX, pid=$OWNER_PID)"
if kill -0 "$OWNER_PID" 2>/dev/null; then
  kill -9 "$OWNER_PID" 2>/dev/null || true
  ok "sent SIGKILL to node $DEMO_OWNER_INDEX (owner of shard 0, hosting the fleet)"
else
  note "owner pid $OWNER_PID already gone (re-run?); continuing to assert survivor state"
fi

# --- ASSERT 1: a survivor's ClusterSupervisor auto-adopted the dead shard. ----
say "ASSERT the LIBRARY auto-failed-over (survivor SS-5b log line)"
ADOPTED=0
for ((s=0; s<NODES; s++)); do
  [ "$s" -eq "$DEMO_OWNER_INDEX" ] && continue
  if wait_for_file_line "$STATE_DIR/logs/node$s.log" "$ADOPT_LINE" 30; then
    ok "node $s auto-adopted the downed owner's shard (shipped SS-5b code)"
    ADOPTED=1; break
  fi
done
[ "$ADOPTED" = 1 ] || die "no survivor logged the SS-5b auto-adoption within 30s"

# --- ASSERT 2: the workflow reaches Completed on a survivor. ------------------
say "ASSERT the fleet workflow completes on a survivor (exactly-once)"
SURV_GRPC=()
for ((s=0; s<NODES; s++)); do
  [ "$s" -eq "$DEMO_OWNER_INDEX" ] && continue
  SURV_GRPC+=("http://127.0.0.1:$(grpc_port "$s")")
done

FINAL=""; FINAL_EP=""
for _ in $(seq 1 120); do  # up to ~60s
  for ep in "${SURV_GRPC[@]}"; do
    st="$(describe_status "$ep" "$WORKLOAD_ID")"
    case "$st" in
      Completed) FINAL="$st"; FINAL_EP="$ep"; break 2 ;;
      Failed|Cancelled|TimedOut) FINAL="$st"; FINAL_EP="$ep"; break 2 ;;
    esac
  done
  sleep 0.5
done

[ "$FINAL" = "Completed" ] \
  || die "fleet workflow did not complete on any survivor after the kill (status=${FINAL:-<unreadable>})"
ok "fleet workflow reached Completed on a survivor ($FINAL_EP)"

# --- ASSERT 3: EXACTLY-ONCE — one ActivityCompleted terminal per ordinal. -----
COUNT="$(activity_completed_count "$FINAL_EP" "$WORKLOAD_ID")"
if [ "$COUNT" = "$FAN_OUT" ]; then
  ok "EXACTLY-ONCE proven: $COUNT/$FAN_OUT ActivityCompleted terminals (no dupes, no loss)"
else
  die "EXACTLY-ONCE VIOLATED: expected $FAN_OUT terminals, found $COUNT"
fi

say "FAILOVER DEMO PASSED"
note "A node was kill -9'd mid-flight; a survivor auto-adopted its shard, the"
note "worker redialed the survivor, and the fan-out completed exactly-once."
note "Watch / inspect: $AION describe $WORKLOAD_ID --endpoint $FINAL_EP"
