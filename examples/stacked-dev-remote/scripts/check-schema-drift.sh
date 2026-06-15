#!/usr/bin/env bash
# Drift gate for the stage-contract schemas (design constraint CN7).
#
# The package copies under schemas/ must stay byte-identical to the
# design-system canon under docs/design-system/schemas/; only the filename
# changes (hyphen -> underscore, ".schema.json" suffix dropped). This script
# encodes exactly that rename mapping, compares each pair with cmp, and exits
# non-zero naming every drifted or missing package schema. It never rewrites
# either side, and it deliberately ignores the hand-owned workflow-IO schemas
# (input.json, output.json, gate_input.json, gate_output.json, ...): CN7
# covers stage contracts only.
#
# Paths are resolved from this script's own location, so it works from any
# working directory.
set -u

# The package test suite points the VM-global PATH at exclusive shim
# directories (CN9), so the system tools this script needs (dirname, cmp)
# are pinned ahead of whatever PATH the caller left behind — the same
# /bin-holds-the-real-tools assumption the shim scripts themselves encode.
export PATH="/usr/bin:/bin:${PATH}"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
package_root="$(cd "${script_dir}/.." && pwd)"
repo_root="$(cd "${package_root}/../.." && pwd)"

package_schemas="${package_root}/schemas"
canon_schemas="${repo_root}/docs/design-system/schemas"

# Fixed rename mapping: package filename -> canon filename.
pairs=(
  "scout_report.json:scout-report.schema.json"
  "dev_report.json:dev-report.schema.json"
  "review_report.json:review-report.schema.json"
)

failed=0

for pair in "${pairs[@]}"; do
  package_name="${pair%%:*}"
  canon_name="${pair##*:}"
  package_file="${package_schemas}/${package_name}"
  canon_file="${canon_schemas}/${canon_name}"

  if [[ ! -f "${canon_file}" ]]; then
    echo "MISSING canon: ${canon_name} (checked against ${package_name})" >&2
    failed=1
    continue
  fi
  if [[ ! -f "${package_file}" ]]; then
    echo "MISSING package schema: ${package_name} (canon ${canon_name})" >&2
    failed=1
    continue
  fi
  if cmp -s "${package_file}" "${canon_file}"; then
    echo "OK: ${package_name} == ${canon_name}"
  else
    echo "DRIFT: ${package_name} differs from canon ${canon_name}" >&2
    failed=1
  fi
done

if [[ "${failed}" -ne 0 ]]; then
  echo "schema drift gate FAILED: package copies must be byte-identical to docs/design-system/schemas/" >&2
  exit 1
fi

echo "schema drift gate passed: all stage-contract schemas byte-identical to canon"
