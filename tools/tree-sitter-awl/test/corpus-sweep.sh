#!/bin/sh
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
grammar_root=$(CDPATH= cd -- "$here/.." && pwd)
repo_root=$(CDPATH= cd -- "$grammar_root/../.." && pwd)
manifest=$(mktemp)
output=$(mktemp)
trap 'rm -f "$manifest" "$output"' EXIT HUP INT TERM

find "$repo_root/crates/aion-awl/tests/fixtures/rev2" \
  -path '*/valid/*.awl' -type f -print | sort > "$manifest"
find "$repo_root/docs/design/aion-authoring/awl/examples/rev2" \
  -name '*.awl' -type f -print | sort >> "$manifest"
printf '%s\n' "$repo_root/examples/awl-hello/awl_hello.awl" >> "$manifest"

count=0
while IFS= read -r file; do
  count=$((count + 1))
  if ! (cd "$grammar_root" && tree-sitter parse "$file") > "$output" 2>&1; then
    cat "$output"
    echo "TS-G1 FAIL: tree-sitter parse failed for $file" >&2
    exit 1
  fi
  if grep -E '\(ERROR|\(MISSING' "$output" >/dev/null; then
    cat "$output"
    echo "TS-G1 FAIL: ERROR or MISSING node in $file" >&2
    exit 1
  fi
done < "$manifest"

echo "TS-G1 PASS: parsed $count valid AWL files with 0 ERROR nodes and 0 MISSING nodes"
