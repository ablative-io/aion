#!/bin/sh
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
grammar_root=$(CDPATH= cd -- "$here/.." && pwd)
repo_root=$(CDPATH= cd -- "$grammar_root/../.." && pwd)
actual=$(mktemp)
trap 'rm -f "$actual"' EXIT HUP INT TERM

cd "$grammar_root"
for file in \
  "$repo_root/docs/design/aion-authoring/awl/examples/rev2/awl_hello.awl" \
  "$repo_root/docs/design/aion-authoring/awl/examples/rev2/dev_brief.awl" \
  "$repo_root/examples/awl-hello/awl_hello.awl"
do
  tree-sitter highlight "$file" >/dev/null
done

tree-sitter query -c queries/highlights.scm \
  ../../examples/awl-hello/awl_hello.awl > "$actual"
if ! cmp -s test/awl_hello.highlights.golden "$actual"; then
  diff -u test/awl_hello.highlights.golden "$actual" || true
  echo "TS-G3 FAIL: awl_hello highlight captures drifted" >&2
  exit 1
fi

echo "TS-G3 PASS: highlighted 3 flagship AWL files; awl_hello capture golden matches"
