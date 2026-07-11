#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
plugin="$repo/editors/nvim-awl"
sample="$repo/docs/design/aion-authoring/awl/examples/rev2/awl_hello.awl"
grammar="$repo/tools/tree-sitter-awl"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/nvim-awl-smoke.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

for file in \
  "$plugin/plugin/awl.lua" \
  "$plugin/lua/awl/init.lua" \
  "$plugin/lsp/awl.lua" \
  "$plugin/ftplugin/awl.lua" \
  "$plugin/tests/smoke.lua"
do
  luac -p "$file"
done

for query in highlights folds indents
do
  tree-sitter query -q -p "$grammar" "$plugin/queries/awl/$query.scm" "$sample"
done

mkdir -p "$test_root/parser"
tree-sitter build -o "$test_root/parser/awl.so" "$grammar"

AION_ROOT="$repo" AWL_PARSER_RUNTIME="$test_root" \
  nvim --headless -u NONE -l "$plugin/tests/smoke.lua"
