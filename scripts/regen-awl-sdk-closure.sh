#!/usr/bin/env bash
# regen-awl-sdk-closure.sh — DEV-TIME regeneration of the embedded AWL SDK
# BEAM closure. Requires a LOCAL gleam toolchain (unless --harvest-only).
#
# What it does:
#   1. Runs `gleam build` in the oracle Gleam project (default:
#      examples/awl-hello) so build/dev/erlang is fresh. Pass --harvest-only
#      to skip the build and harvest an existing built tree instead
#      (refused when the tree is older than manifest.toml — a dependency
#      bump without a rebuild must not stamp a new version on stale beams).
#   2. Computes the production dependency closure TRANSITIVELY — seeded from
#      gleam.toml [dependencies], expanded over manifest.toml requirements,
#      mirroring production_closure() in
#      crates/aion-package/src/project/discover.rs — and copies those
#      packages' BEAMs into crates/aion-awl-package/sdk-closure/, excluding the
#      SDK test-only modules (aion_flow_ffi, aion@testing, aion@testing@*)
#      exactly as the legacy discovery filter does. No package list is
#      hard-coded: a new transitive dependency of aion_flow is picked up by
#      the next regeneration automatically.
#   3. Regenerates crates/aion-awl-package/src/bundle_data.rs: a sorted
#      include_bytes! table plus the aion_flow version stamp read from the
#      oracle project's manifest.toml lockfile.
#
# When to run: whenever gleam/aion_flow (or its gleam_json / gleam_stdlib
# pins) changes. Commit the regenerated sdk-closure/ tree and bundle_data.rs
# together — bundle-integrity tests pin the closure checksum and will fail
# until the pin is updated deliberately.
#
# The PRODUCT path never invokes this script, and no build.rs does: the
# committed closure ships embedded in the compiled binary via include_bytes!.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT="$REPO_ROOT/examples/awl-hello"
HARVEST_ONLY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --harvest-only) HARVEST_ONLY=1 ;;
    --from)
      shift
      PROJECT="$1"
      ;;
    *)
      echo "usage: $0 [--harvest-only] [--from <gleam project root>]" >&2
      exit 2
      ;;
  esac
  shift
done

ERLANG="$PROJECT/build/dev/erlang"
CLOSURE_DIR="$REPO_ROOT/crates/aion-awl-package/sdk-closure"
GENERATED="$REPO_ROOT/crates/aion-awl-package/src/bundle_data.rs"

if [ "$HARVEST_ONLY" -eq 0 ]; then
  (cd "$PROJECT" && gleam build)
elif [ ! -d "$ERLANG" ]; then
  echo "error: --harvest-only but $ERLANG does not exist" >&2
  exit 1
elif [ -z "$(find "$ERLANG" -name '*.beam' -newer "$PROJECT/manifest.toml" -print | head -n 1)" ]; then
  # A dependency bump refreshes manifest.toml; harvesting a tree built
  # BEFORE that refresh would stamp the new version over stale beams.
  echo "error: --harvest-only but every .beam under $ERLANG is older than" >&2
  echo "       $PROJECT/manifest.toml — run gleam build first (stale harvest refused)" >&2
  exit 1
fi

SDK_VERSION="$(sed -n 's/.*name = "aion_flow", version = "\([^"]*\)".*/\1/p' "$PROJECT/manifest.toml" | head -1)"
if [ -z "$SDK_VERSION" ]; then
  echo "error: could not read aion_flow version from $PROJECT/manifest.toml" >&2
  exit 1
fi

# --- production dependency closure, computed transitively -------------------
# Seeds: the oracle project's gleam.toml [dependencies] (production only —
# dev-dependencies never enter). Expansion: manifest.toml `requirements`
# lists, followed to a fixed point. This mirrors production_closure() in
# crates/aion-package/src/project/discover.rs so the embedded bundle can
# never silently lag a new transitive SDK dependency.
production_dep_seeds() {
  awk '
    /^\[dependencies\]$/ { in_deps = 1; next }
    /^\[/ { in_deps = 0 }
    in_deps && /^[a-z0-9_]+[ \t]*=/ {
      name = $0
      sub(/[ \t]*=.*/, "", name)
      print name
    }
  ' "$PROJECT/gleam.toml"
}

requirements_of() {
  sed -n "s/.*{ name = \"$1\",.*requirements = \[\([^]]*\)\].*/\1/p" \
    "$PROJECT/manifest.toml" | head -1 | tr -d '",'
}

PACKAGES=""
QUEUE="$(production_dep_seeds | tr '\n' ' ')"
while [ -n "${QUEUE// /}" ]; do
  # shellcheck disable=SC2086
  set -- $QUEUE
  pkg="$1"
  shift
  QUEUE="$*"
  case " $PACKAGES " in *" $pkg "*) continue ;; esac
  if ! grep -q "{ name = \"$pkg\"," "$PROJECT/manifest.toml"; then
    echo "error: dependency $pkg has no manifest.toml entry (unresolved lockfile?)" >&2
    exit 1
  fi
  PACKAGES="$PACKAGES $pkg"
  QUEUE="$QUEUE $(requirements_of "$pkg")"
done
PACKAGES="$(printf '%s\n' $PACKAGES | LC_ALL=C sort | tr '\n' ' ')"

case " $PACKAGES " in
  *" aion_flow "*) ;;
  *)
    echo "error: aion_flow is not in the oracle project's production closure" >&2
    exit 1
    ;;
esac
echo "production closure:$PACKAGES" | tr -s ' '

excluded() {
  case "$1" in
    aion_flow_ffi | aion@testing | aion@testing@*) return 0 ;;
    *) return 1 ;;
  esac
}

rm -rf "$CLOSURE_DIR"
ROWS=""
# shellcheck disable=SC2086
for package in $PACKAGES; do
  ebin="$ERLANG/$package/ebin"
  if [ ! -d "$ebin" ]; then
    echo "error: missing $ebin (is the project built?)" >&2
    exit 1
  fi
  mkdir -p "$CLOSURE_DIR/$package"
  for beam in "$ebin"/*.beam; do
    module="$(basename "$beam" .beam)"
    if [ "$package" = aion_flow ] && excluded "$module"; then
      continue
    fi
    cp "$beam" "$CLOSURE_DIR/$package/$module.beam"
    ROWS="$ROWS$module\t$package\n"
  done
done

{
  echo "//! GENERATED by scripts/regen-awl-sdk-closure.sh — DO NOT EDIT BY HAND."
  echo "//!"
  echo "//! Embedded AWL SDK BEAM closure: the oracle project's production"
  echo "//! dependency closure, computed transitively from \`gleam.toml\` +"
  echo "//! \`manifest.toml\` ($(echo "$PACKAGES" | tr -s ' ' | sed 's/^ //;s/ $//;s/ /, /g;s/[a-z_][a-z_0-9]*/\`&\`/g')),"
  echo "//! SDK test-only modules excluded, harvested from a built Gleam tree."
  echo "//! Regenerate with the script above whenever the SDK changes."
  echo
  echo "/// \`aion_flow\` version the embedded closure was built from."
  echo "pub(crate) const SDK_CLOSURE_VERSION: &str = \"$SDK_VERSION\";"
  echo
  echo "/// (logical module name, exact \`.beam\` bytes), sorted by module name."
  echo "pub(crate) const MODULES: &[(&str, &[u8])] = &["
  printf '%b' "$ROWS" | LC_ALL=C sort | while IFS="$(printf '\t')" read -r module package; do
    echo "    ("
    echo "        \"$module\","
    echo "        include_bytes!(\"../sdk-closure/$package/$module.beam\"),"
    echo "    ),"
  done
  echo "];"
} >"$GENERATED"

count="$(printf '%b' "$ROWS" | wc -l | tr -d ' ')"
echo "regenerated: $count modules, aion_flow $SDK_VERSION"
echo "  closure:   $CLOSURE_DIR"
echo "  generated: $GENERATED"
