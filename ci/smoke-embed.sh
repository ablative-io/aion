#!/usr/bin/env bash
# Smoke check: the release `aion` binary (built with --features release, which
# turns on aion-server/embed-dashboard) must serve the REAL dashboard at `/`,
# not the committed placeholder stub. Fails loudly if:
#   * the placeholder marker (AION_EMBED_PLACEHOLDER) is present at `/`, or
#   * `/` carries no built asset reference (/assets/index-*.js).
#
# This is the "release that forgot the embed feature / forgot the pipeline"
# guard: either omission makes `/` serve the placeholder, which fails here.
set -euo pipefail

BIN="${AION_BIN:-target/release/aion}"
HTTP_PORT="${AION_SMOKE_HTTP_PORT:-18099}"
GRPC_PORT="${AION_SMOKE_GRPC_PORT:-51099}"
HTTP_ADDR="127.0.0.1:${HTTP_PORT}"

if [[ ! -x "$BIN" ]]; then
  echo "::error::release binary not found at $BIN (build with --features release first)" >&2
  exit 1
fi

CONFIG="$(mktemp)"
cat >"$CONFIG" <<EOF
workflow_packages = []

[server]
listen_address = "${HTTP_ADDR}"
grpc_address = "127.0.0.1:${GRPC_PORT}"

[store]
backend = "memory"

[runtime]
query_timeout_ms = 10000

[namespaces]
default = "default"

[websocket]
event_broadcast_capacity = 1024
EOF

"$BIN" server --config "$CONFIG" &
SRV=$!
cleanup() {
  kill "$SRV" 2>/dev/null || true
  rm -f "$CONFIG"
}
trap cleanup EXIT

# Wait for the listener.
for _ in $(seq 1 100); do
  if [[ "$(curl -s -o /dev/null -w '%{http_code}' "http://${HTTP_ADDR}/" 2>/dev/null)" == "200" ]]; then
    break
  fi
  sleep 0.2
done

BODY="$(curl -s "http://${HTTP_ADDR}/")"

if grep -q "AION_EMBED_PLACEHOLDER" <<<"$BODY"; then
  echo "::error::/ served the committed PLACEHOLDER stub — the embed pipeline did not run, or the embed feature is off." >&2
  echo "--- served body ---" >&2
  echo "$BODY" >&2
  exit 1
fi

if ! grep -qE '/assets/index-[A-Za-z0-9_]+\.(js|css)' <<<"$BODY"; then
  echo "::error::/ has no built asset reference — the embedded bundle is not the real dashboard." >&2
  echo "--- served body ---" >&2
  echo "$BODY" >&2
  exit 1
fi

echo "OK: release binary serves the real dashboard at / (asset references present, no placeholder)."
