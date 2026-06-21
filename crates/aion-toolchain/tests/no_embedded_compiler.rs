//! Proves CN7 / C15 / R1 acceptance #3 mechanically: the `aion-toolchain`
//! crate embeds no Gleam compiler — its resolved dependency tree carries no
//! Gleam compiler crate (and no `beamr` BEAM VM).
//!
//! The toolchain compiles Gleam only by spawning the external `gleam` binary;
//! nothing about the compiler is linked in. This test walks the full resolved
//! dependency closure of `aion-toolchain` from the workspace `Cargo.lock` and
//! asserts that none of the names that would indicate an embedded Gleam
//! compiler (or the beamr VM) appear.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Package names whose presence anywhere in the toolchain's dependency closure
/// would mean a Gleam compiler (or the BEAM VM) is embedded — the exact thing
/// CN7 forbids.
const FORBIDDEN: &[&str] = &["gleam", "gleam-core", "gleam_core", "glistix", "beamr"];

#[derive(serde::Deserialize)]
struct Lockfile {
    #[serde(default, rename = "package")]
    packages: Vec<LockPackage>,
}

#[derive(serde::Deserialize, Clone)]
struct LockPackage {
    name: String,
    #[serde(default)]
    dependencies: Vec<String>,
}

fn workspace_lock_path() -> PathBuf {
    // crates/aion-toolchain -> workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock")
}

/// Resolves the full transitive dependency closure of `root` over the
/// lockfile's `dependencies` edges (names only; version disambiguation is
/// unnecessary for a presence check).
fn closure(packages: &[LockPackage], root: &str) -> BTreeSet<String> {
    let edges: BTreeMap<&str, &[String]> = packages
        .iter()
        .map(|package| (package.name.as_str(), package.dependencies.as_slice()))
        .collect();
    let mut seen = BTreeSet::new();
    let mut queue = vec![root.to_owned()];
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(deps) = edges.get(name.as_str()) {
            for dependency in *deps {
                // Lock dependency strings may carry a version/source suffix
                // ("name 1.2.3 (registry+...)"); the bare name leads.
                let bare = dependency.split_whitespace().next().unwrap_or(dependency);
                queue.push(bare.to_owned());
            }
        }
    }
    seen
}

#[test]
fn no_gleam_compiler_in_dependency_tree() -> TestResult {
    let lock_path = workspace_lock_path();
    let text = std::fs::read_to_string(&lock_path)?;
    let lockfile: Lockfile = toml::from_str(&text)?;

    // Sanity: the lockfile must actually contain this crate, or the closure
    // walk proves nothing.
    assert!(
        lockfile
            .packages
            .iter()
            .any(|package| package.name == "aion-toolchain"),
        "aion-toolchain not found in {}; the dependency-set proof cannot run",
        lock_path.display()
    );

    let reachable = closure(&lockfile.packages, "aion-toolchain");
    for forbidden in FORBIDDEN {
        assert!(
            !reachable.contains(*forbidden),
            "CN7 violation: `{forbidden}` is in aion-toolchain's dependency closure — the toolchain must embed no Gleam compiler and must only spawn the external `gleam` binary"
        );
    }
    Ok(())
}
