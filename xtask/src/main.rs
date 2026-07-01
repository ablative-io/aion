//! Repo automation tasks.
//!
//! Run via the workspace alias: `cargo xtask <task>`.
//!
//! Tasks:
//! * `build-ops-console` — the embed pipeline. In order it (a) regenerates the
//!   ts-rs wire types, (b) `bun install && bun run build` in `apps/aion-ops-console`,
//!   and (c) syncs `apps/aion-ops-console/dist/*` into
//!   `crates/aion-server/ops-console-embed/`. The ops console is ALWAYS embedded (no
//!   cargo feature), so a plain `cargo build` ships the real UI; this task just
//!   refreshes the committed bundle.
//! * `verify-ops-console` — CI freshness guard. It rebuilds the ops console into a
//!   scratch directory and DIFFs the result against the committed
//!   `crates/aion-server/ops-console-embed/`. If they differ (file set or
//!   contents), it exits non-zero telling the dev to run `cargo xtask
//!   build-ops-console` and commit. Vite output hashes are content-derived, so a
//!   clean rebuild of unchanged source reproduces the committed bundle exactly.
//! * `no-norn-in-platform` — the §3A.4 INVARIANT gate (NOI-4). Asserts that the
//!   aion PLATFORM crate set carries ZERO Norn-specific coupling: (1) no
//!   cargo-tree dependency edge to `aion-integration-norn` (or a `norn*` crate)
//!   from any platform crate, and (2) no `norn`-crate identifier (`use norn` /
//!   `norn::` / `aion_integration_norn` / `extern crate norn`) in platform `src`,
//!   excluding the allowlisted doc-comments and `#[cfg(test)]` routing fixtures.
//!   Fails non-zero the instant a Norn edge is added to a platform crate, so the
//!   invariant stops holding by discipline and starts holding by construction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("build-ops-console") => build_ops_console(),
        Some("verify-ops-console") => verify_ops_console(),
        Some("no-norn-in-platform") => no_norn_in_platform(),
        Some(other) => {
            print_usage();
            bail!("unknown task `{other}`");
        }
        None => {
            print_usage();
            bail!("no task given");
        }
    }
}

fn print_usage() {
    eprintln!(
        "cargo xtask <task>\n\n\
         tasks:\n\
         \x20 build-ops-console     regenerate wire types, build the ops console, sync into ops-console-embed/\n\
         \x20 verify-ops-console    rebuild the ops console to a scratch dir and assert it matches the committed bundle\n\
         \x20 no-norn-in-platform   assert the platform crates carry no Norn dependency edge or identifier (§3A.4)"
    );
}

/// The embed pipeline: build the ops console and sync it into the committed
/// `ops-console-embed/` bundle.
fn build_ops_console() -> Result<()> {
    let root = workspace_root()?;
    let dist_dir = build_ops_console_dist(&root)?;
    let embed_dir = root.join("crates/aion-server/ops-console-embed");

    // Sync dist/* into ops-console-embed/. The embed dir is wiped (except its
    // dotfiles like .gitignore) and repopulated so no stale asset lingers.
    step("syncing dist -> crates/aion-server/ops-console-embed");
    sync_embed(&dist_dir, &embed_dir)?;

    let index = embed_dir.join("index.html");
    if !index.is_file() {
        bail!(
            "embed sync did not produce `index.html` at `{}`",
            index.display()
        );
    }

    eprintln!(
        "\nembed pipeline complete. The ops console is ALWAYS embedded — a plain\n\
         `cargo build -p aion-cli` now ships the real UI at `/`. Commit the\n\
         refreshed `crates/aion-server/ops-console-embed/` bundle."
    );
    Ok(())
}

/// CI freshness guard: rebuild the ops console and assert the result matches the
/// committed bundle byte-for-byte (file set + contents). Exits non-zero with a
/// clear remediation message on any drift.
fn verify_ops_console() -> Result<()> {
    let root = workspace_root()?;
    let dist_dir = build_ops_console_dist(&root)?;
    let embed_dir = root.join("crates/aion-server/ops-console-embed");

    step("diffing fresh build against committed ops-console-embed/");
    let fresh = bundle_files(&dist_dir)?;
    // The committed bundle includes `.gitignore`, which the build does not
    // produce; ignore dotfiles when comparing so only built assets are checked.
    let committed = bundle_files(&embed_dir)?;
    let committed: BTreeMap<String, Vec<u8>> = committed
        .into_iter()
        .filter(|(name, _)| !name.starts_with('.') && !is_ignored_artifact(name))
        .collect();

    if fresh == committed {
        eprintln!(
            "OK: committed ops-console-embed/ matches a clean rebuild ({} files).",
            fresh.len()
        );
        return Ok(());
    }

    // Collect diff lines into a Vec and join, rather than push_str(&format!(..))
    // (clippy::format_push_string) or write!-into-String (a must-use Copy
    // `fmt::Result` that can't be unwrap/expect'd under the deny lints).
    let mut diff_lines: Vec<String> = Vec::new();
    for name in fresh.keys() {
        match committed.get(name) {
            None => diff_lines.push(format!("  + {name} (built, not committed)")),
            Some(bytes) if bytes != &fresh[name] => {
                diff_lines.push(format!("  ~ {name} (contents differ)"));
            }
            Some(_) => {}
        }
    }
    for name in committed.keys() {
        if !fresh.contains_key(name) {
            diff_lines.push(format!("  - {name} (committed, no longer built)"));
        }
    }
    let diff = diff_lines.join("\n");

    bail!(
        "committed ops-console-embed/ is STALE against a clean rebuild:\n{diff}\n\
         Run `cargo xtask build-ops-console` and commit the refreshed bundle."
    );
}

/// A built asset that should never be committed even if it appears in the embed
/// dir (mirrors `ops-console-embed/.gitignore`'s `*.map`).
fn is_ignored_artifact(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("map"))
}

/// Run the shared build steps (regen wire types + bun build) and return the
/// `apps/aion-ops-console/dist` path. Used by both `build-ops-console` and
/// `verify-ops-console` so the two always build identically.
fn build_ops_console_dist(root: &Path) -> Result<PathBuf> {
    let ops_console_dir = root.join("apps/aion-ops-console");
    let dist_dir = ops_console_dir.join("dist");

    if !ops_console_dir.is_dir() {
        bail!(
            "ops console app not found at `{}`",
            ops_console_dir.display()
        );
    }

    // (a) Regenerate the ts-rs wire types from Rust-owned types. This is the
    // existing exporter test; it writes apps/aion-ops-console/src/types/generated/.
    step("regenerating ts-rs wire types");
    run(Command::new("cargo")
        .args(["test", "-p", "aion-core", "export_ops_console_wire_types"])
        .current_dir(root))
    .context("ts-rs wire type export failed")?;

    // (b) Build the ops console bundle with bun.
    step("bun install");
    run(Command::new("bun")
        .arg("install")
        .current_dir(&ops_console_dir))
    .context("`bun install` failed (is bun installed?)")?;

    step("bun run build");
    run(Command::new("bun")
        .args(["run", "build"])
        .current_dir(&ops_console_dir))
    .context("`bun run build` failed")?;

    if !dist_dir.is_dir() {
        bail!(
            "ops console build produced no `dist/` at `{}`",
            dist_dir.display()
        );
    }

    Ok(dist_dir)
}

/// Read every file under `dir` into a name -> bytes map keyed by path relative
/// to `dir` (forward-slash separated), for content comparison.
fn bundle_files(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    collect_files(dir, dir, &mut files)?;
    Ok(files)
}

fn collect_files(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading `{}`", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .with_context(|| format!("`{}` not under `{}`", path.display(), root.display()))?;
            let key = rel.to_string_lossy().replace('\\', "/");
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading `{}`", path.display()))?;
            files.insert(key, bytes);
        }
    }
    Ok(())
}

/// Replace the contents of `embed_dir` with a copy of `dist_dir`, preserving any
/// dotfiles already in `embed_dir` (notably `.gitignore`).
fn sync_embed(dist_dir: &Path, embed_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(embed_dir)
        .with_context(|| format!("creating `{}`", embed_dir.display()))?;

    // Clear prior built assets but keep dotfiles (.gitignore).
    for entry in std::fs::read_dir(embed_dir)
        .with_context(|| format!("reading `{}`", embed_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("removing `{}`", path.display()))?;
        } else {
            std::fs::remove_file(&path)
                .with_context(|| format!("removing `{}`", path.display()))?;
        }
    }

    copy_tree(dist_dir, embed_dir)
}

/// Recursively copy every entry under `src` into `dst`.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating `{}`", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading `{}`", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copying `{}` -> `{}`", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn step(message: &str) {
    eprintln!("[xtask] {message}");
}

/// Run a command, inheriting stdio, and fail if it exits non-zero.
fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to spawn `{command:?}`"))?;
    if !status.success() {
        bail!("command `{command:?}` exited with {status}");
    }
    Ok(())
}

/// The workspace root: the parent of this crate's directory (`xtask/`).
fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .context("xtask crate has no parent directory")
}

// --------------------------------------------------------------------------- //
// no-norn-in-platform — the §3A.4 invariant gate (NOI-4).
// --------------------------------------------------------------------------- //

/// The PLATFORM crate set (§3A.4): the library crates + the `aion` facade that
/// MUST stay Norn-blind. NOT `aion-cli` (the composition root, allowed to compose
/// the adapter), NOT `examples`, NOT `aion-integration-norn`/`aion-integrations`.
const PLATFORM_CRATES: &[&str] = &[
    "aion-core",
    "aion-worker",
    "aion-server",
    "aion-proto",
    "aion-proto-generated",
    "aion-store",
    "aion-store-haematite",
    "aion-store-libsql",
    "aion-client",
    "aion-rs", // the `aion` facade crate (package name `aion-rs`, dir `crates/aion`).
];

/// The concrete Norn packages a platform crate must never depend on. `norn`
/// itself is not a workspace/registry dependency (the adapter shells out to the
/// binary), so `aion-integration-norn` is the real edge to guard; `norn` is listed
/// defensively for the day a real `norn` crate could appear.
const NORN_DEP_SPECS: &[&str] = &["aion-integration-norn", "norn"];

/// Directory (relative to a crate root) → filename → line numbers that are
/// ALLOWLISTED to mention `norn` in prose or as a `#[cfg(test)]` routing fixture
/// (§3A.4). A whole file may be allowlisted with an empty line set meaning "all
/// lines". These are prose/doc-comments and test-only `task_queue` strings — never
/// a crate dependency or a `use` of a norn crate.
///
/// The scan below only flags a norn *crate coupling* (`use norn`, `norn::`,
/// `aion_integration_norn`, `extern crate norn`), so ordinary prose like "there is
/// no Norn here" or a `"norn"` `task_queue` string never trips it; this allowlist is
/// the belt-and-braces record of the doc-named exceptions.
fn allowlisted_files() -> BTreeMap<&'static str, &'static str> {
    // Files whose norn mentions are entirely prose/doc-comment or test fixtures.
    // Keyed by path relative to the workspace root.
    let mut map = BTreeMap::new();
    map.insert(
        "crates/aion-server/src/worker/registry.rs",
        "doc-comment task_queue example (line 70) + #[cfg(test)] routing fixtures",
    );
    map.insert(
        "crates/aion-server/src/worker/liminal_transport.rs",
        "#[cfg(test)] channel-name routing fixtures",
    );
    map.insert(
        "crates/aion-core/src/activity_event.rs",
        "doc-comment: 'there is no Norn here' (the invariant, in prose)",
    );
    map.insert(
        "crates/aion-core/src/intervention.rs",
        "doc-comment: neutral-vocabulary rationale naming Norn in prose",
    );
    map.insert(
        "crates/aion-worker/src/runtime/agent.rs",
        "doc-comment: names the run_norn_step example + 'Norn, or a future one' in prose",
    );
    map.insert(
        "crates/aion-worker/src/runtime/agent_tests.rs",
        "doc-comment: 'NO norn' — the test double names no norn crate",
    );
    map
}

/// A norn-CRATE coupling detected in platform src: a `use`/path/extern of a norn
/// crate. Ordinary prose and `"norn"` strings are NOT couplings and are not
/// matched.
struct NornCoupling {
    file: String,
    line: usize,
    text: String,
}

fn no_norn_in_platform() -> Result<()> {
    let root = workspace_root()?;
    step("no-norn-in-platform: (1) dependency-edge check via cargo tree");
    let edge_failures = check_no_dependency_edges(&root)?;
    step("no-norn-in-platform: (2) identifier check via scoped src scan");
    let identifier_failures = check_no_norn_identifiers(&root)?;

    if edge_failures.is_empty() && identifier_failures.is_empty() {
        eprintln!(
            "OK: no norn dependency edge and no norn-crate identifier in any of the {} platform crates.",
            PLATFORM_CRATES.len()
        );
        return Ok(());
    }

    let mut report: Vec<String> = Vec::new();
    if !edge_failures.is_empty() {
        report.push("DEPENDENCY EDGES (a platform crate depends on a norn crate):".to_owned());
        for failure in &edge_failures {
            report.push(format!("  - {failure}"));
        }
    }
    if !identifier_failures.is_empty() {
        report.push("NORN-CRATE IDENTIFIERS in platform src:".to_owned());
        for coupling in &identifier_failures {
            report.push(format!(
                "  - {}:{}: {}",
                coupling.file,
                coupling.line,
                coupling.text.trim()
            ));
        }
    }
    let report = report.join("\n");
    bail!(
        "no-norn-in-platform gate FAILED — the §3A.4 invariant is violated:\n{report}\n\
         The platform library crates must never depend on or name a norn crate; the \
         Norn adapter is composed only at the aion-cli binary root."
    );
}

/// For each platform crate and each norn dep spec, run `cargo tree -p CRATE -i
/// SPEC`. A dependency EDGE exists iff the command succeeds AND prints a tree
/// naming the spec; a "did not match any packages" error means the spec is not in
/// that crate's resolved graph (no edge — a PASS). Returns the human descriptions
/// of every offending edge.
fn check_no_dependency_edges(root: &Path) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for crate_name in PLATFORM_CRATES {
        for spec in NORN_DEP_SPECS {
            let output = Command::new("cargo")
                .args(["tree", "-p", crate_name, "-i", spec])
                .current_dir(root)
                .output()
                .with_context(|| format!("running `cargo tree -p {crate_name} -i {spec}`"))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            if output.status.success() && stdout.contains(spec) {
                failures.push(format!(
                    "`cargo tree -p {crate_name} -i {spec}` reports an edge:\n{}",
                    stdout.trim()
                ));
            }
            // A non-zero exit ("did not match any packages") == no edge == PASS.
        }
    }
    Ok(failures)
}

/// Scan every platform crate's `src/` for a norn-CRATE identifier: `use norn` /
/// `norn::` / `aion_integration_norn` / `extern crate norn`. Prose, doc-comments,
/// and `"norn"` `task_queue` strings are deliberately NOT matched. Files on the
/// allowlist are skipped (their norn mentions are the doc-named exceptions).
fn check_no_norn_identifiers(root: &Path) -> Result<Vec<NornCoupling>> {
    let allowlist = allowlisted_files();
    let mut couplings = Vec::new();
    for crate_name in PLATFORM_CRATES {
        let dir = crate_dir(root, crate_name).join("src");
        if !dir.is_dir() {
            continue;
        }
        scan_dir_for_couplings(root, &dir, &allowlist, &mut couplings)?;
    }
    Ok(couplings)
}

/// Map a platform crate package name to its source directory. The `aion` facade's
/// package name is `aion-rs` but its directory is `crates/aion`.
fn crate_dir(root: &Path, crate_name: &str) -> PathBuf {
    let dir_name = if crate_name == "aion-rs" {
        "aion"
    } else {
        crate_name
    };
    root.join("crates").join(dir_name)
}

fn scan_dir_for_couplings(
    root: &Path,
    dir: &Path,
    allowlist: &BTreeMap<&'static str, &'static str>,
    couplings: &mut Vec<NornCoupling>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading `{}`", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            scan_dir_for_couplings(root, &path, allowlist, couplings)?;
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if allowlist.contains_key(rel.as_str()) {
            continue;
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("reading `{}`", path.display()))?;
        for (index, line) in contents.lines().enumerate() {
            if line_couples_to_norn_crate(line) {
                couplings.push(NornCoupling {
                    file: rel.clone(),
                    line: index + 1,
                    text: line.to_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Whether a source line references a norn CRATE (a real coupling), as opposed to
/// merely containing the word "norn" in prose or a string. Matches:
/// `use norn`/`use aion_integration_norn`, a `norn::`/`aion_integration_norn::`
/// path segment, or `extern crate norn`. Excludes `run_norn`, `norn_tx`, `"norn"`,
/// and `aion-integration-norn` (the hyphenated Cargo name in a comment/string).
fn line_couples_to_norn_crate(line: &str) -> bool {
    // A leading `//` or `//!`/`///` doc-comment line is prose, never a coupling.
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return false;
    }
    references_norn_crate_path(line)
}

/// Detects a `norn` or `aion_integration_norn` CRATE path/use/extern in a code
/// line. Uses byte scanning (no regex dep) to require crate-token boundaries so
/// `run_norn_step` / `norn_tx` / `"norn"` do not match.
fn references_norn_crate_path(line: &str) -> bool {
    // `aion_integration_norn` as an identifier (only appears as the adapter crate).
    if contains_ident(line, "aion_integration_norn") {
        return true;
    }
    // `extern crate norn`.
    if line.contains("extern crate norn") {
        return true;
    }
    // `use norn` / `use norn::...` — a crate import.
    for hay in [line.trim_start()] {
        if let Some(rest) = hay.strip_prefix("use ") {
            let first = rest.trim_start();
            if first == "norn" || first.starts_with("norn::") || first.starts_with("norn;") {
                return true;
            }
        }
    }
    // A `norn::` path segment as a standalone crate token (boundary before it must
    // not be an identifier char — excludes `run_norn::`-style false positives,
    // though none exist — and it must be immediately followed by `::`).
    contains_crate_path(line, "norn")
}

/// Whether `ident` appears in `line` as a whole identifier (both sides bounded by
/// a non-identifier byte).
fn contains_ident(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = ident.as_bytes();
    let mut start = 0;
    while let Some(pos) = find_from(bytes, needle, start) {
        let before_ok = pos == 0 || !is_ident_byte(bytes[pos - 1]);
        let after = pos + needle.len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// Whether `crate_name::` appears as a crate path token: bounded on the left by a
/// non-identifier byte and immediately followed by `::`.
fn contains_crate_path(line: &str, crate_name: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = format!("{crate_name}::");
    let needle = needle.as_bytes();
    let mut start = 0;
    while let Some(pos) = find_from(bytes, needle, start) {
        let before_ok = pos == 0 || !is_ident_byte(bytes[pos - 1]);
        if before_ok {
            return true;
        }
        start = pos + 1;
    }
    false
}

fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|pos| pos + from)
}

fn is_ident_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}
