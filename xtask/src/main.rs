//! Repo automation tasks.
//!
//! Run via the workspace alias: `cargo xtask <task>`.
//!
//! Tasks:
//! * `build-dashboard` — the embed pipeline. In order it (a) regenerates the
//!   ts-rs wire types, (b) `bun install && bun run build` in `apps/aion-dashboard`,
//!   and (c) syncs `apps/aion-dashboard/dist/*` into
//!   `crates/aion-server/dashboard-embed/`. The dashboard is ALWAYS embedded (no
//!   cargo feature), so a plain `cargo build` ships the real UI; this task just
//!   refreshes the committed bundle.
//! * `verify-dashboard` — CI freshness guard. It rebuilds the dashboard into a
//!   scratch directory and DIFFs the result against the committed
//!   `crates/aion-server/dashboard-embed/`. If they differ (file set or
//!   contents), it exits non-zero telling the dev to run `cargo xtask
//!   build-dashboard` and commit. Vite output hashes are content-derived, so a
//!   clean rebuild of unchanged source reproduces the committed bundle exactly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("build-dashboard") => build_dashboard(),
        Some("verify-dashboard") => verify_dashboard(),
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
         \x20 build-dashboard    regenerate wire types, build the dashboard, sync into dashboard-embed/\n\
         \x20 verify-dashboard   rebuild the dashboard to a scratch dir and assert it matches the committed bundle"
    );
}

/// The embed pipeline: build the dashboard and sync it into the committed
/// `dashboard-embed/` bundle.
fn build_dashboard() -> Result<()> {
    let root = workspace_root()?;
    let dist_dir = build_dashboard_dist(&root)?;
    let embed_dir = root.join("crates/aion-server/dashboard-embed");

    // Sync dist/* into dashboard-embed/. The embed dir is wiped (except its
    // dotfiles like .gitignore) and repopulated so no stale asset lingers.
    step("syncing dist -> crates/aion-server/dashboard-embed");
    sync_embed(&dist_dir, &embed_dir)?;

    let index = embed_dir.join("index.html");
    if !index.is_file() {
        bail!(
            "embed sync did not produce `index.html` at `{}`",
            index.display()
        );
    }

    eprintln!(
        "\nembed pipeline complete. The dashboard is ALWAYS embedded — a plain\n\
         `cargo build -p aion-cli` now ships the real UI at `/`. Commit the\n\
         refreshed `crates/aion-server/dashboard-embed/` bundle."
    );
    Ok(())
}

/// CI freshness guard: rebuild the dashboard and assert the result matches the
/// committed bundle byte-for-byte (file set + contents). Exits non-zero with a
/// clear remediation message on any drift.
fn verify_dashboard() -> Result<()> {
    let root = workspace_root()?;
    let dist_dir = build_dashboard_dist(&root)?;
    let embed_dir = root.join("crates/aion-server/dashboard-embed");

    step("diffing fresh build against committed dashboard-embed/");
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
            "OK: committed dashboard-embed/ matches a clean rebuild ({} files).",
            fresh.len()
        );
        return Ok(());
    }

    let mut diff = String::new();
    for name in fresh.keys() {
        match committed.get(name) {
            None => diff.push_str(&format!("  + {name} (built, not committed)\n")),
            Some(bytes) if bytes != &fresh[name] => {
                diff.push_str(&format!("  ~ {name} (contents differ)\n"));
            }
            Some(_) => {}
        }
    }
    for name in committed.keys() {
        if !fresh.contains_key(name) {
            diff.push_str(&format!("  - {name} (committed, no longer built)\n"));
        }
    }

    bail!(
        "committed dashboard-embed/ is STALE against a clean rebuild:\n{diff}\n\
         Run `cargo xtask build-dashboard` and commit the refreshed bundle."
    );
}

/// A built asset that should never be committed even if it appears in the embed
/// dir (mirrors `dashboard-embed/.gitignore`'s `*.map`).
fn is_ignored_artifact(name: &str) -> bool {
    name.ends_with(".map")
}

/// Run the shared build steps (regen wire types + bun build) and return the
/// `apps/aion-dashboard/dist` path. Used by both `build-dashboard` and
/// `verify-dashboard` so the two always build identically.
fn build_dashboard_dist(root: &Path) -> Result<PathBuf> {
    let dashboard_dir = root.join("apps/aion-dashboard");
    let dist_dir = dashboard_dir.join("dist");

    if !dashboard_dir.is_dir() {
        bail!("dashboard app not found at `{}`", dashboard_dir.display());
    }

    // (a) Regenerate the ts-rs wire types from Rust-owned types. This is the
    // existing exporter test; it writes apps/aion-dashboard/src/types/generated/.
    step("regenerating ts-rs wire types");
    run(Command::new("cargo")
        .args(["test", "-p", "aion-core", "export_dashboard_wire_types"])
        .current_dir(root))
    .context("ts-rs wire type export failed")?;

    // (b) Build the dashboard bundle with bun.
    step("bun install");
    run(Command::new("bun")
        .arg("install")
        .current_dir(&dashboard_dir))
    .context("`bun install` failed (is bun installed?)")?;

    step("bun run build");
    run(Command::new("bun")
        .args(["run", "build"])
        .current_dir(&dashboard_dir))
    .context("`bun run build` failed")?;

    if !dist_dir.is_dir() {
        bail!(
            "dashboard build produced no `dist/` at `{}`",
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
    eprintln!("[xtask build-dashboard] {message}");
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
