#!/usr/bin/env bash
#
# gen-cluster-config.sh — standalone cluster-config GENERATOR for the Aion
# failover demo. Emits a valid per-node aion.toml set for an N-node
# haematite+liminal cluster, in single-laptop (loopback) or real-IP (shared)
# mode. The launcher calls emit_cluster_config from lib.sh directly; this script
# exposes the same generator on its own so you can inspect / version configs.
#
# Usage:
#   scripts/demo/gen-cluster-config.sh --out DIR --package FILE.aion \
#       [--nodes N] [--mode loopback|shared] [--host IP]
#
# Examples:
#   # 3-node single-laptop config set into ./demo-cfg
#   scripts/demo/gen-cluster-config.sh --out demo-cfg \
#       --package /tmp/fleet.aion --nodes 3
#
#   # real-IP / Tailscale: every node binds 100.x.y.z (one node per host)
#   DEMO_HOSTS="100.0.0.1 100.0.0.2 100.0.0.3" \
#     scripts/demo/gen-cluster-config.sh --out cfg --package fleet.aion \
#       --nodes 3 --mode shared
#
# Ports: bind 7700+, grpc 50051+, http 8090+, liminal 8190+ (per node index).
# TCP 7000 is deliberately avoided (macOS AirPlay Receiver).

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/demo/lib.sh
source "$ROOT/scripts/demo/lib.sh"

OUT=""; PACKAGE=""; NODES=3; MODE="loopback"; HOST="127.0.0.1"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out)     OUT="${2:-}"; shift 2 ;;
    --package) PACKAGE="${2:-}"; shift 2 ;;
    --nodes)   NODES="${2:-}"; shift 2 ;;
    --mode)    MODE="${2:-}"; shift 2 ;;
    --host)    HOST="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[ -n "$OUT" ] || die "--out DIR is required"
[ -n "$PACKAGE" ] || die "--package FILE.aion is required"
case "$MODE" in loopback|shared) ;; *) die "--mode must be loopback or shared" ;; esac
[ "$NODES" -ge 3 ] || note "WARNING: <3 nodes cannot survive a kill (quorum needs >=3)"

# Resolve the package to an absolute path so generated configs are portable.
PACKAGE="$(cd "$(dirname "$PACKAGE")" && pwd)/$(basename "$PACKAGE")"
[ -f "$PACKAGE" ] || die "package not found: $PACKAGE"

emit_cluster_config "$OUT" "$NODES" "$PACKAGE" "$HOST" "$MODE"

say "generated $NODES-node config set in $OUT (mode=$MODE)"
for ((i=0; i<NODES; i++)); do
  note "node$i.toml  http=$(http_port "$i") grpc=$(grpc_port "$i") bind=$(bind_port "$i") liminal=$(liminal_port "$i") owns shard $i"
done
