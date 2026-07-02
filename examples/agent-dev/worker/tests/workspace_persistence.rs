//! Workspace-lifecycle tests for the clone path — the #175 properties reused
//! from the stacked-dev-remote worker, driven against a REAL `git` on `PATH`.
//!
//! The #175 defect these pin against: provisioning into a volatile OS temp
//! directory, recording the path in durable workflow history, and losing
//! every unpushed dev-round commit to a reboot or temp-reaper. The loss-repro
//! test provisions from a local fixture repository, commits a simulated dev
//! round in the workspace, simulates the reboot purge (delete the workspace
//! IFF it lives under a volatile temp root), then runs a later activity step
//! (`land`) against the recorded `workspace.path` and requires both the step
//! to succeed and the commit to be intact.
//!
//! Companion tests pin the lifecycle contract: the provisioned path lives
//! under the configured stable root (never a volatile root), a colliding
//! run_id-keyed directory (this execution's own earlier partial attempt) is
//! renamed aside — never reused or deleted — so crash recovery via reopen
//! re-provisions with the same id, and a genuinely missing workspace fails
//! with the explicit "workspace missing" diagnostic.
//!
//! Unlike the stacked-dev-remote suite this never touches the process
//! environment: the agent-dev worker resolves the workspace root ONCE at its
//! composition root and THREADS it into `provision`, so the tests hand the
//! stable root in directly.

#![cfg(unix)]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use agent_dev_worker::handlers;
use agent_dev_worker::shell::Shell;
use agent_dev_worker::types::{GateInput, LandInput, ProvisionInput};

type TestResult = Result<(), Box<dyn Error>>;

static WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// The stable workspace root for this test binary. Lives under
/// `CARGO_TARGET_TMPDIR` — inside the crate's own `target/`, which is NOT a
/// volatile OS temp root, so the simulated reboot purge must leave it alone.
fn stable_root() -> &'static Path {
    WORKSPACE_ROOT.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("agent-dev-workspace-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        root
    })
}

/// Run one git command in `dir`, failing the test loudly on a non-zero exit.
fn git(dir: &Path, args: &[&str]) -> TestResult {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

/// Build a tiny committed fixture repository to clone from.
fn fixture_repo(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fixture-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("README.md"), "# agent-dev loss-repro fixture\n")?;
    git(&dir, &["init", "-b", "main"])?;
    git(&dir, &["config", "user.email", "loss-repro@test"])?;
    git(&dir, &["config", "user.name", "Loss Repro"])?;
    git(&dir, &["add", "-A"])?;
    git(&dir, &["commit", "-m", "fixture: initial"])?;
    Ok(dir)
}

/// Whether `path` lives under a volatile root a reboot or temp-reaper purges:
/// the OS temp dir, `/tmp`, or `/var/folders` (macOS per-user temp),
/// including their `/private` realpaths.
fn is_under_volatile_root(path: &Path) -> bool {
    let mut roots = vec![std::env::temp_dir()];
    roots.extend(
        [
            "/tmp",
            "/var/folders",
            "/private/tmp",
            "/private/var/folders",
        ]
        .map(PathBuf::from),
    );
    roots.iter().any(|root| path.starts_with(root))
}

/// Simulate the OS reboot / temp-reaper purge: delete the directory IFF it
/// lives under a volatile temp root. A workspace under the stable root
/// survives untouched — exactly what a reboot does.
fn purge_if_volatile(path: &Path) -> TestResult {
    if is_under_volatile_root(path) && path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

/// A provision input against the fixture repository, keyed by a stable
/// per-run id — the workflow id string the `agent_dev` workflow passes in the
/// activity input.
fn clone_input(fixture: &Path, brief_id: &str, run_id: &str) -> ProvisionInput {
    ProvisionInput {
        repo_url: fixture.to_string_lossy().into_owned(),
        base_ref: "main".to_owned(),
        brief_id: brief_id.to_owned(),
        run_id: run_id.to_owned(),
    }
}

// --- the loss repro -----------------------------------------------------------

/// #175 loss repro: (a) provision from a clone URL, (b) commit a dev round in
/// the workspace, (c) simulate the reboot purge of volatile temp roots, (d)
/// run a later activity step (`land`) against the recorded `workspace.path` —
/// the step must succeed and the dev-round commit must be intact.
#[test]
fn workspace_survives_reboot_purge_and_later_activity_succeeds() -> TestResult {
    let root = stable_root();
    let fixture = fixture_repo("survival")?;
    let shell = Shell::inherited();

    // (a) provision.
    let workspace = handlers::provision(
        &shell,
        root,
        clone_input(&fixture, "loss-repro", "run-survival"),
    )
    .map_err(|failure| format!("provision failed: {}", failure.message()))?;
    let workspace_dir = PathBuf::from(&workspace.path);
    assert_eq!(workspace.branch, "agent-dev-loss-repro");

    // (b) a simulated dev round: real, unpushed, UNCOMMITTED work the later
    // land step commits.
    git(&workspace_dir, &["config", "user.email", "loss-repro@test"])?;
    git(&workspace_dir, &["config", "user.name", "Loss Repro"])?;
    std::fs::write(
        workspace_dir.join("DEV-ROUND.md"),
        "unpushed dev-round work\n",
    )?;

    // (c) the reboot: volatile temp roots are purged; durable state (the
    // workflow history carrying `workspace.path`) survives.
    purge_if_volatile(&workspace_dir)?;

    // (d) the later activity step re-dispatched against the recorded path.
    let landed = handlers::land(
        &shell,
        LandInput {
            workspace: workspace.clone(),
            brief_id: "loss-repro".to_owned(),
        },
    )
    .map_err(|failure| {
        format!(
            "later activity failed against the recorded workspace path — \
             the reboot purge lost the clone and its dev-round work: {}",
            failure.message()
        )
    })?;
    assert_eq!(
        landed.commit_sha.len(),
        40,
        "land must return the full commit sha; got {:?}",
        landed.commit_sha
    );

    // The dev-round commit is intact and carries the contract message.
    let log = std::process::Command::new("git")
        .args(["log", "--format=%H %s"])
        .current_dir(&workspace_dir)
        .output()?;
    let subjects = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(
        subjects.contains(&format!("{} agent-dev: loss-repro", landed.commit_sha)),
        "the landed commit must survive with the contract message; got log: {subjects}"
    );

    // The recorded path must live under the stable root — never under a
    // volatile temp root a reboot would purge.
    assert!(
        workspace_dir.starts_with(root),
        "workspace {} must live under the stable root {}",
        workspace.path,
        root.display()
    );
    assert!(
        !is_under_volatile_root(&workspace_dir),
        "workspace {} must not live under a volatile temp root",
        workspace.path
    );
    Ok(())
}

// --- lifecycle contract -------------------------------------------------------

/// A colliding run_id-keyed directory is this execution's own earlier partial
/// provision attempt (run ids are unique per execution; recorded successes
/// are never re-executed on reopen). Re-provisioning renames the stale
/// attempt aside — its contents survive intact, nothing is deleted — and
/// proceeds fresh, so a worker killed mid-clone recovers through reopen with
/// the SAME run id instead of wedging terminally.
#[test]
fn run_id_collision_renames_the_stale_attempt_aside_and_reprovisions() -> TestResult {
    let root = stable_root();
    let fixture = fixture_repo("collision")?;
    let shell = Shell::inherited();

    let stale = root.join("run-collision");
    std::fs::create_dir_all(&stale)?;
    let sentinel = stale.join("partial-clone.txt");
    std::fs::write(&sentinel, "half a clone from the killed attempt\n")?;

    let workspace = handlers::provision(
        &shell,
        root,
        clone_input(&fixture, "collision", "run-collision"),
    )
    .map_err(|failure| {
        format!(
            "re-provision after a collision failed: {}",
            failure.message()
        )
    })?;

    let expected = root.join("run-collision").join("repo");
    assert_eq!(PathBuf::from(&workspace.path), expected);
    assert!(
        expected.join(".git").is_dir(),
        "the fresh clone must be real"
    );

    // The stale attempt survives, renamed aside — never deleted.
    let renamed: Vec<PathBuf> = std::fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("run-collision.superseded-")
            })
        })
        .collect();
    assert_eq!(
        renamed.len(),
        1,
        "exactly one renamed stale attempt; got {renamed:?}"
    );
    assert!(
        renamed[0].join("partial-clone.txt").exists(),
        "the stale attempt's contents must survive the rename intact"
    );
    Ok(())
}

/// An activity dispatched against a genuinely missing workspace fails with
/// the explicit missing-workspace diagnostic — never a confusing downstream
/// CLI error.
#[test]
fn missing_workspace_fails_with_the_explicit_diagnostic() {
    let root = stable_root();
    let dead = root.join("run-never-provisioned").join("repo");

    let failure = handlers::gate(
        &Shell::inherited(),
        GateInput {
            path: dead.to_string_lossy().into_owned(),
        },
    )
    .err()
    .map(|failure| failure.message().to_owned())
    .unwrap_or_default();

    assert!(
        failure.contains("workspace missing at") && failure.contains("run cannot resume"),
        "the diagnostic must name the lost workspace; got: {failure}"
    );
}
