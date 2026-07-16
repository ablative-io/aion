//! Freshness guard for the committed SDK closure: rebuild the oracle Gleam
//! project (`examples/awl-hello`, the exact tree `regen-awl-sdk-closure.sh`
//! harvests) and export-set-compare the fresh harvest against the committed
//! `sdk-closure/` beams — the same rebuild-and-compare pattern as the
//! ops-console embed freshness guard.
//!
//! A stale committed bundle (an SDK edit without a closure regeneration)
//! fails here in CI instead of failing at runtime in a deployed direct
//! archive. Comparison is by export SET, not bytes: a compiler upgrade may
//! legitimately change beam bytes without changing the wire surface, and the
//! surface is what generated code links against. Requires a local `gleam`
//! toolchain, exactly like the AWL emitter compile-proof tests.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl_package::{sdk_closure_modules, sdk_closure_version};
use beamr::atom::AtomTable;
use beamr::loader::load_beam_chunks;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// SDK test-only modules the regeneration script excludes from the
/// `aion_flow` package (mirrors `excluded()` in
/// `scripts/regen-awl-sdk-closure.sh`).
fn excluded_sdk_module(package: &str, module: &str) -> bool {
    package == "aion_flow"
        && (module == "aion_flow_ffi"
            || module == "aion@testing"
            || module.starts_with("aion@testing@"))
}

/// Exported `(function, arity)` set of one beam file.
fn export_set(bytes: &[u8]) -> Result<BTreeSet<(String, u32)>, Box<dyn std::error::Error>> {
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(bytes, &atoms)?;
    let mut set = BTreeSet::new();
    for export in &parsed.exports {
        let function = atoms
            .resolve(export.function)
            .ok_or_else(|| format!("unresolvable export atom: {:?}", export.function))?;
        set.insert((function.to_owned(), u32::from(export.arity)));
    }
    Ok(set)
}

/// `aion_flow`'s locked version in the oracle project's lockfile.
fn oracle_sdk_version(manifest: &str) -> Result<String, Box<dyn std::error::Error>> {
    for line in manifest.lines() {
        if let Some(rest) = line
            .trim()
            .strip_prefix("{ name = \"aion_flow\", version = \"")
            && let Some((version, _)) = rest.split_once('"')
        {
            return Ok(version.to_owned());
        }
    }
    Err("oracle manifest.toml has no aion_flow entry".into())
}

/// Rebuild the oracle project and compare the fresh harvest with the
/// committed closure: same package set, same module set (modulo the script's
/// SDK test-only exclusions), same per-module export sets, and the committed
/// version stamp equals the oracle's locked `aion_flow` version.
#[test]
fn committed_closure_matches_a_fresh_oracle_harvest() -> TestResult {
    let root = workspace_root();
    let oracle = root.join("examples/awl-hello");
    let output = Command::new("gleam")
        .arg("build")
        .current_dir(&oracle)
        .output()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("gleam binary is required for the SDK closure freshness guard: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "oracle `gleam build` failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let manifest = fs::read_to_string(oracle.join("manifest.toml"))?;
    assert_eq!(
        oracle_sdk_version(&manifest)?,
        sdk_closure_version(),
        "the committed closure's version stamp lags the oracle's locked aion_flow version — \
         run scripts/regen-awl-sdk-closure.sh and commit the result"
    );

    // Fresh harvest: every built production package (everything under
    // build/dev/erlang except the oracle project itself) and its module set.
    let erlang = oracle.join("build/dev/erlang");
    let mut fresh: BTreeSet<String> = BTreeSet::new();
    let mut fresh_beams: Vec<(String, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&erlang)? {
        let entry = entry?;
        let package = entry.file_name().to_string_lossy().into_owned();
        if package == "awl_hello" {
            continue;
        }
        let ebin = entry.path().join("ebin");
        if !ebin.is_dir() {
            continue;
        }
        for beam in fs::read_dir(&ebin)? {
            let beam = beam?.path();
            if beam.extension().is_none_or(|extension| extension != "beam") {
                continue;
            }
            let Some(module) = beam
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
            else {
                continue;
            };
            if excluded_sdk_module(&package, &module) {
                continue;
            }
            fresh.insert(module.clone());
            fresh_beams.push((module, beam));
        }
    }

    let committed: BTreeSet<String> = sdk_closure_modules()
        .map(|(name, _)| name.to_owned())
        .collect();
    let missing: Vec<&String> = fresh.difference(&committed).collect();
    let stale: Vec<&String> = committed.difference(&fresh).collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "committed closure module set differs from a fresh harvest — run \
         scripts/regen-awl-sdk-closure.sh and commit the result.\n  fresh-only: {missing:?}\n  \
         committed-only: {stale:?}"
    );

    // Export-set equality per module: the wire surface generated code links
    // against must be exactly what ships.
    let mut skewed = Vec::new();
    for (module, path) in fresh_beams {
        let fresh_exports = export_set(&fs::read(&path)?)?;
        let committed_bytes = sdk_closure_modules()
            .find(|(name, _)| *name == module)
            .map(|(_, bytes)| bytes)
            .ok_or_else(|| format!("module `{module}` vanished from the committed closure"))?;
        let committed_exports = export_set(committed_bytes)?;
        if fresh_exports != committed_exports {
            let gained: Vec<String> = fresh_exports
                .difference(&committed_exports)
                .map(|(function, arity)| format!("{function}/{arity}"))
                .collect();
            let lost: Vec<String> = committed_exports
                .difference(&fresh_exports)
                .map(|(function, arity)| format!("{function}/{arity}"))
                .collect();
            skewed.push(format!("{module} (+{gained:?} -{lost:?})"));
        }
    }
    assert!(
        skewed.is_empty(),
        "committed closure export sets lag the SDK source — run \
         scripts/regen-awl-sdk-closure.sh and commit the result: {skewed:?}"
    );
    Ok(())
}
