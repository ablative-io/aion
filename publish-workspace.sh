#!/usr/bin/env bash
#
# publish-workspace.sh — publish the aion crates to crates.io, in dependency
# order, with retries and skip-if-already-uploaded. Safe to re-run: a crate
# already at this version is treated as done, so a crash partway through just
# resumes on the next run (or the next retry).
#
# Usage:
#   ./publish-workspace.sh            # verify build (safe default), publish all
#   NO_VERIFY=1 ./publish-workspace.sh  # skip per-crate verify build (faster, riskier)
#   DRY_RUN=1 ./publish-workspace.sh    # cargo publish --dry-run for every crate
#
# Run in the background and watch the log:
#   nohup ./publish-workspace.sh > publish.log 2>&1 &
#   tail -f publish.log
#
# Notes:
# - aion_flow (Gleam/hex) is NOT handled here — it's a separate `gleam publish`
#   and is unchanged at 0.4.0 this release.
# - Needs a crates.io token: `cargo login <token>` or CARGO_REGISTRY_TOKEN set.
# - cargo 1.92 waits for each crate to propagate to the index before returning,
#   so ordering is handled; the retry loop covers rate-limits (429), 5xx, and
#   any residual index lag.
# - AFTER publishing aion-worker: bump the standalone example crates' crates.io
#   pins (examples/stacked-dev/*, examples/stacked-dev-remote/worker,
#   .meridian/workflows/stacked-dev/worker) and refresh their lockfiles, or the
#   CI standalone-crates lane goes red on the release PR (checklist C15,
#   docs/design/aion-publish/checklist.json).

set -u -o pipefail

# Dependency order: a crate only publishes after everything it depends on.
CRATES=(
  aion-core
  aion-proto-generated
  aion-store
  aion-package
  aion-proto
  aion-nif
  aion-store-libsql
  aion-store-haematite
  aion-toolchain
  aion-integrations
  aion-integration-norn
  aion-awl
  aion-awl-lsp
  aion-awl-package
  aion-rs            # directory crates/aion, package name aion-rs
  aion-worker
  aion-integration-cli
  aion-client
  aion-server
  aion-cli
)

MAX_ATTEMPTS=8          # per crate
BASE_BACKOFF=15         # seconds; grows each attempt

cd "$(dirname "$0")" || { echo "cannot cd to script dir"; exit 1; }

VERSION="$(grep -m1 '^version = ' Cargo.toml | sed 's/version = //; s/"//g')"
echo "=== aion workspace publish — version ${VERSION} — $(date -u +%FT%TZ) ==="

# --- preflight -------------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
  echo "FATAL: cargo not found on PATH"; exit 1
fi
if [[ -z "${CARGO_REGISTRY_TOKEN:-}" ]] && ! grep -qs 'token' "${CARGO_HOME:-$HOME/.cargo}/credentials.toml" "${CARGO_HOME:-$HOME/.cargo}/credentials" 2>/dev/null; then
  echo "WARNING: no crates.io token detected (CARGO_REGISTRY_TOKEN unset and no"
  echo "         ~/.cargo/credentials[.toml]). Run 'cargo login <token>' first,"
  echo "         or publishes will fail with an auth error (which this script"
  echo "         will then retry pointlessly). Continuing anyway."
fi

PUBLISH_FLAGS=()
[[ -n "${NO_VERIFY:-}" ]] && PUBLISH_FLAGS+=(--no-verify)
[[ -n "${DRY_RUN:-}"  ]] && PUBLISH_FLAGS+=(--dry-run)

# --- publish one crate with retries ---------------------------------------
publish_one() {
  local crate="$1" attempt=1 out rc backoff
  while (( attempt <= MAX_ATTEMPTS )); do
    echo "--- [$crate] attempt ${attempt}/${MAX_ATTEMPTS} $(date -u +%FT%TZ) ---"
    # Capture output so we can recognise the already-published case.
    out="$(cargo publish -p "$crate" ${PUBLISH_FLAGS[@]+"${PUBLISH_FLAGS[@]}"} 2>&1)"
    rc=$?
    echo "$out"

    if (( rc == 0 )); then
      echo "+++ [$crate] published ok"
      return 0
    fi

    # Idempotency: a version already on crates.io is success, not failure.
    if grep -qiE 'already (uploaded|exists)|is already uploaded|already published' <<<"$out"; then
      echo "=== [$crate] already at ${VERSION} on crates.io — skipping"
      return 0
    fi

    # Anything else (rate-limit 429, 5xx, transient index/network) — back off.
    backoff=$(( BASE_BACKOFF * attempt ))
    if grep -qiE '429|rate.?limit|too many requests' <<<"$out"; then
      backoff=$(( backoff + 60 ))   # crates.io publish rate-limit: wait longer
      echo "!!! [$crate] rate-limited; backing off ${backoff}s"
    else
      echo "!!! [$crate] failed (rc=$rc); backing off ${backoff}s then retrying"
    fi
    sleep "$backoff"
    (( attempt++ ))
  done
  echo "XXX [$crate] still failing after ${MAX_ATTEMPTS} attempts — STOPPING."
  echo "    Fix the cause, then re-run this script: already-published crates are"
  echo "    skipped, so it resumes from ${crate}."
  return 1
}

# --- drive -----------------------------------------------------------------
for crate in "${CRATES[@]}"; do
  if ! publish_one "$crate"; then
    echo "=== ABORTED at ${crate} — $(date -u +%FT%TZ) ==="
    exit 1
  fi
done

echo "=== all ${#CRATES[@]} crates published at ${VERSION} — $(date -u +%FT%TZ) ==="
echo "    Remember: aion_flow (hex) is separate — publish it from gleam/aion_flow with gleam publish."
