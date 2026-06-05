#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PACKAGE_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)

FIXTURES="wrong_activity_input wrong_signal_payload wrong_query_return"

expected_substring() {
  case "$1" in
    wrong_activity_input) printf '%s' 'Expected type:' ;;
    wrong_signal_payload) printf '%s' 'Expected type:' ;;
    wrong_query_return) printf '%s' 'Expected type:' ;;
    *) printf '%s\n' "unknown fixture: $1" >&2; exit 2 ;;
  esac
}

make_project() {
  fixture_file="$1"
  tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/aion-flow-compile-fail.XXXXXX")
  mkdir -p "$tmp_dir/src"
  cat > "$tmp_dir/gleam.toml" <<EOF
name = "compile_fail_fixture"
version = "0.1.0"
target = "erlang"

[dependencies]
aion_flow = { path = "$PACKAGE_ROOT" }
gleam_stdlib = ">= 0.34.0 and < 2.0.0"
EOF
  cp "$fixture_file" "$tmp_dir/src/compile_fail_fixture.gleam"
  printf '%s' "$tmp_dir"
}

check_positive() {
  fixture="$1"
  project_dir=$(make_project "$SCRIPT_DIR/${fixture}_positive.gleam")
  if output=$(cd "$project_dir" && gleam check 2>&1); then
    printf 'ok: %s_positive.gleam type-checks\n' "$fixture"
  else
    printf 'not ok: %s_positive.gleam should type-check\n%s\n' "$fixture" "$output" >&2
    exit 1
  fi
}

check_negative() {
  fixture="$1"
  project_dir=$(make_project "$SCRIPT_DIR/${fixture}_negative.gleam")
  expected=$(expected_substring "$fixture")
  if output=$(cd "$project_dir" && gleam check 2>&1); then
    printf 'not ok: %s_negative.gleam unexpectedly type-checked\n' "$fixture" >&2
    exit 1
  fi

  case "$output" in
    *"$expected"*)
      printf 'ok: %s_negative.gleam fails with documented type mismatch (%s)\n' "$fixture" "$expected"
      ;;
    *)
      printf 'not ok: %s_negative.gleam failed without expected substring: %s\n%s\n' "$fixture" "$expected" "$output" >&2
      exit 1
      ;;
  esac
}

for fixture in $FIXTURES; do
  check_positive "$fixture"
  check_negative "$fixture"
done
