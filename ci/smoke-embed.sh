#!/usr/bin/env bash
# Smoke check: the `aion` binary — built with a PLAIN `cargo build` (no features:
# the ops console is always embedded) — must serve the REAL ops console at `/`.
# Fails loudly if:
#   * a stale committed placeholder marker (AION_EMBED_PLACEHOLDER) is present, or
#   * `/` carries no built asset reference (/assets/index-*.js).
#
# This guards against a stale or missing committed bundle: either makes `/` serve
# something other than the real built UI, which fails here. Point AION_BIN at the
# built binary (e.g. target/debug/aion or target/release/aion).
set -euo pipefail

BIN="${AION_BIN:-target/debug/aion}"
HTTP_PORT="${AION_SMOKE_HTTP_PORT:-18099}"
GRPC_PORT="${AION_SMOKE_GRPC_PORT:-51099}"
HTTP_ADDR="127.0.0.1:${HTTP_PORT}"

if [[ ! -x "$BIN" ]]; then
  echo "::error::aion binary not found at $BIN (run a plain \`cargo build -p aion-cli\` first)" >&2
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
  echo "::error::/ served a PLACEHOLDER stub — the committed ops-console-embed/ bundle is stale. Run \`cargo xtask build-ops-console\` and commit." >&2
  echo "--- served body ---" >&2
  echo "$BODY" >&2
  exit 1
fi

if ! grep -qE '/assets/index-[A-Za-z0-9_]+\.(js|css)' <<<"$BODY"; then
  echo "::error::/ has no built asset reference — the embedded bundle is not the real ops console." >&2
  echo "--- served body ---" >&2
  echo "$BODY" >&2
  exit 1
fi

echo "OK: binary serves the real ops console at / (asset references present, no placeholder)."
