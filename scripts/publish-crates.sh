#!/usr/bin/env bash
# Publish Aion crates in dependency order.
#
# crates.io requires dependencies to be available before crates that depend on
# them. Keep this leaf-first order in sync with README.md and validate it with
# cargo metadata before every dry-run or live publish.
set -euo pipefail

readonly SCRIPT_NAME="$(basename "$0")"
readonly MODE_DRY_RUN="dry-run"
readonly MODE_LIVE="live"

readonly PUBLISH_ORDER=(
  aion-core
  aion-store
  aion-store-libsql
  aion-package
  aion-nif
  aion
  aion-proto
  aion-server
  aion-worker
  aion-client
  aion-cli
)

usage() {
  cat <<USAGE
Usage: ${SCRIPT_NAME} [--live] [--help]

Publishes the Aion Rust crates in leaf-first dependency order:
  ${PUBLISH_ORDER[*]}

By default, this performs a full safe dry run:
  cargo publish --dry-run -p <crate>

Options:
  --live    publish for real with cargo publish -p <crate>
  -h, --help
            show this help text
USAGE
}

mode="${MODE_DRY_RUN}"
for arg in "$@"; do
  case "$arg" in
    --live)
      mode="${MODE_LIVE}"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: ${arg}" >&2
      usage >&2
      exit 2
      ;;
  esac
done

find_repo_root() {
  local root
  root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "error: ${SCRIPT_NAME} must be run from inside the Aion git repository" >&2
    exit 1
  }
  printf '%s\n' "$root"
}

validate_publish_order() {
  local metadata
  local jq_program
  local order_json
  local order_errors

  if ! command -v jq >/dev/null 2>&1; then
    echo "error: jq is required to validate the publish order from cargo metadata" >&2
    exit 1
  fi

  echo "==> Validating publish order with cargo metadata"
  metadata="$(cargo metadata --format-version=1 --no-deps)"
  order_json="$(printf '%s\n' "${PUBLISH_ORDER[@]}" | jq --raw-input --slurp 'split("\n")[:-1]')"

  jq_program='def idx($order): reduce range(0; $order | length) as $i ({}; . + {($order[$i]): $i});
    . as $metadata
    | idx($order) as $index
    | ($metadata.packages | map(select(.name as $name | $index[$name] != null)) | length) as $found
    | if $found != ($order | length) then
        ($metadata.packages | map(.name) | unique) as $packages
        | ($order | map(select(. as $name | ($packages | index($name)) == null))) as $missing
        | "missing publish-order crate(s) from cargo metadata: \($missing | join(", "))"
      else
        [ $metadata.packages[]
          | select(.name as $name | $index[$name] != null)
          | .name as $dependent
          | .dependencies[]?
          | select(.source == null)
          | select(.name as $dependency | $index[$dependency] != null)
          | select($index[.name] >= $index[$dependent])
          | "\($dependent) depends on \(.name), but \(.name) appears later in PUBLISH_ORDER"
        ]
        | if length > 0 then .[] else empty end
      end'

  order_errors="$(jq --raw-output --argjson order "$order_json" "$jq_program" <<<"$metadata")"

  if [[ -n "$order_errors" ]]; then
    printf 'error: publish order validation failed:\n' >&2
    while IFS= read -r order_error; do
      printf '  - %s\n' "$order_error" >&2
    done <<<"$order_errors"
    exit 1
  fi

  echo "==> Publish order validation passed"
}

run_publish() {
  local crate
  local status
  local -a publish_args

  if [[ "$mode" == "${MODE_DRY_RUN}" ]]; then
    publish_args=(--dry-run)
  else
    publish_args=()
    cat <<'WARNING'
==> LIVE publish mode enabled; crates will be published to crates.io.
WARNING
  fi

  for crate in "${PUBLISH_ORDER[@]}"; do
    echo "==> Publishing ${crate} (${mode})"
    if cargo publish -p "$crate" "${publish_args[@]}"; then
      echo "==> Published ${crate} (${mode})"
    else
      status=$?
      echo "error: publishing ${crate} (${mode}) failed with exit status ${status}" >&2
      exit "$status"
    fi
  done
}

main() {
  local repo_root
  repo_root="$(find_repo_root)"
  cd "$repo_root"

  validate_publish_order
  run_publish
}

main "$@"
