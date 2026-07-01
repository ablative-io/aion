//! PACKAGING-INTEGRITY GUARD (#153): the ops console must ship in the PUBLISHED
//! crate, not just in a local workspace checkout.
//!
//! The ops console is served entirely from the compile-time embedded
//! `ops-console-embed/` bundle, so a plain `cargo install aion-cli` (from the
//! crates.io tarball, with NO bun/node/vite on the end-user machine) must produce
//! a binary whose embedded bundle contains `index.html` plus its hashed assets.
//!
//! `cargo package` selects a crate's file set from git tracking (honouring any
//! `include`/`exclude` in `Cargo.toml`). If the embed bundle were ever excluded —
//! a stray `exclude`, an over-broad `.gitignore`, or the assets slipping out of
//! git tracking — the published `.crate` would omit them, `rust_embed` would
//! compile an empty/partial bundle, and `cargo install` would silently yield an
//! API-only (broken-console) binary. Neither the in-crate embed tests (which read
//! the on-disk folder during a workspace `cargo test`) nor `cargo xtask
//! verify-ops-console` (which needs `bun` and never runs on an end user's machine)
//! can catch that regression.
//!
//! This test asks cargo itself which files it would package (`cargo package
//! --list`) and asserts the embed bundle — `index.html` and at least one hashed
//! `assets/` file — is present in that set.

use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `cargo package --list` for `aion-server` must include the embedded ops-console
/// bundle, so the published crate carries the real UI that `cargo install` embeds.
#[test]
fn published_crate_includes_ops_console_bundle() -> TestResult {
    let cargo = env!("CARGO");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let output = Command::new(cargo)
        .args([
            "package",
            "--list",
            "--offline",
            "--allow-dirty",
            "--manifest-path",
        ])
        .arg(format!("{manifest_dir}/Cargo.toml"))
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "`cargo package --list` failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
        .into());
    }

    let listing = String::from_utf8(output.stdout)?;
    let packaged: Vec<&str> = listing.lines().map(str::trim).collect();

    if !packaged.contains(&"ops-console-embed/index.html") {
        return Err(format!(
            "published aion-server crate is MISSING `ops-console-embed/index.html`: a \
             `cargo install` would ship an API-only binary with no ops console. \
             Packaged files:\n{listing}"
        )
        .into());
    }

    let has_hashed_asset = packaged.iter().any(|path| {
        path.starts_with("ops-console-embed/assets/")
            && std::path::Path::new(path)
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("js") || ext.eq_ignore_ascii_case("css")
                })
    });

    if !has_hashed_asset {
        return Err(format!(
            "published aion-server crate ships no `ops-console-embed/assets/*.{{js,css}}` \
             bundle: the embedded ops console would be non-functional after `cargo \
             install`. Packaged files:\n{listing}"
        )
        .into());
    }

    Ok(())
}
