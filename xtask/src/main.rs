//! Repo automation tasks.
//!
//! Run via the workspace alias: `cargo xtask <task>`.
//!
//! Tasks:
//! * `build-dashboard` — the WS5 embed pipeline. In order it (a) regenerates the
//!   ts-rs wire types, (b) `bun install && bun run build` in `apps/aion-dashboard`,
//!   and (c) syncs `apps/aion-dashboard/dist/*` into
//!   `crates/aion-server/dashboard-embed/`. After it runs, build the server with
//!   `--features embed-dashboard` (or aion-cli `--features release`) to ship the
//!   real UI inside the binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("build-dashboard") => build_dashboard(),
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
         \x20 build-dashboard   regenerate wire types, build the dashboard, sync into dashboard-embed/"
    );
}

/// The WS5 embed pipeline.
fn build_dashboard() -> Result<()> {
    let root = workspace_root()?;
    let dashboard_dir = root.join("apps/aion-dashboard");
    let dist_dir = dashboard_dir.join("dist");
    let embed_dir = root.join("crates/aion-server/dashboard-embed");

    if !dashboard_dir.is_dir() {
        bail!(
            "dashboard app not found at `{}`",
            dashboard_dir.display()
        );
    }

    // (a) Regenerate the ts-rs wire types from Rust-owned types. This is the
    // existing exporter test; it writes apps/aion-dashboard/src/types/generated/.
    step("regenerating ts-rs wire types");
    run(
        Command::new("cargo")
            .args(["test", "-p", "aion-core", "export_dashboard_wire_types"])
            .current_dir(&root),
    )
    .context("ts-rs wire type export failed")?;

    // (b) Build the dashboard bundle with bun.
    step("bun install");
    run(Command::new("bun").arg("install").current_dir(&dashboard_dir))
        .context("`bun install` failed (is bun installed?)")?;

    step("bun run build");
    run(
        Command::new("bun")
            .args(["run", "build"])
            .current_dir(&dashboard_dir),
    )
    .context("`bun run build` failed")?;

    if !dist_dir.is_dir() {
        bail!(
            "dashboard build produced no `dist/` at `{}`",
            dist_dir.display()
        );
    }

    // (c) Sync dist/* into dashboard-embed/. The embed dir is wiped (except its
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
        "\nembed pipeline complete. Now build with the dashboard embedded:\n\
         \x20 cargo build -p aion-server --features embed-dashboard\n\
         \x20 cargo build -p aion-cli --release --features release"
    );
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
    for entry in
        std::fs::read_dir(src).with_context(|| format!("reading `{}`", src.display()))?
    {
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
