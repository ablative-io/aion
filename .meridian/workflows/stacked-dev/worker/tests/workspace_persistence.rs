//! Workspace-lifecycle tests for the remote clone path (#175).
//!
//! The defect: remote provisioning cloned into `/tmp/stacked-dev-clones/…`
//! (volatile), recorded the path in durable workflow history, and its
//! collision probing fell back to `rm -rf` of a surviving directory on
//! exhaustion. A host reboot or temp-reaper deleted the clone; resume then
//! re-dispatched activities against the dead path and every unpushed
//! dev-round commit was lost.
//!
//! These tests pin the replacement contract — including the #175 loss
//! repro (provision, commit a dev round, simulate the reboot purge of
//! volatile temp roots, require a later activity step to succeed with the
//! commit intact): remote clones live at `<workspace root>/<run id>/repo`
//! under `AION_WORKSPACE_ROOT` (default `~/.aion/clones`, never a volatile
//! temp root), a colliding run_id-keyed directory (this execution's own
//! earlier partial attempt) is renamed aside — never reused or deleted — so
//! crash recovery via reopen re-provisions with the same id, run_id-less
//! payloads get a unique per-attempt directory and never collide with a
//! salvaged one, path traversal in the run key is refused, teardown refuses
//! paths outside the (canonicalized) root, and a missing workspace fails
//! with the explicit "workspace missing" diagnostic.

#![cfg(unix)]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use stacked_dev_worker::handlers;
use stacked_dev_worker::shell::Shell;
use stacked_dev_worker::types::{
    Isolation, Placement, ProvisionInput, ScoutInput, StartupResult, StartupTask, TeardownInput,
    Workspace,
};

type TestResult = Result<(), Box<dyn Error>>;

static WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// The stable workspace root for this test binary, exported once through
/// `AION_WORKSPACE_ROOT`. Lives under `CARGO_TARGET_TMPDIR` — inside the
/// crate's own `target/`, which is NOT a volatile OS temp root.
#[allow(unsafe_code)]
fn stable_root() -> &'static Path {
    WORKSPACE_ROOT.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("aion-workspace-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        // SAFETY: every test in this binary reaches the environment only
        // through `stable_root()` as its first action, and `get_or_init`
        // blocks concurrent callers until this single initialisation
        // completes, so no environment read races the write.
        unsafe {
            std::env::set_var("AION_WORKSPACE_ROOT", &root);
        }
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

/// Build a tiny committed cargo-crate fixture repository to clone from. The
/// crate shape lets the warm-build activity genuinely succeed post-purge in
/// the loss repro.
fn fixture_repo(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fixture-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src"))?;
    // `[workspace]` keeps cargo from walking up into this worker's own
    // manifest when the fixture is built from under `target/tmp`.
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )?;
    std::fs::write(dir.join("src/lib.rs"), "//! Loss-repro fixture crate.\n")?;
    git(&dir, &["init", "-b", "main"])?;
    git(&dir, &["config", "user.email", "loss-repro@test"])?;
    git(&dir, &["config", "user.name", "Loss Repro"])?;
    git(&dir, &["add", "-A"])?;
    git(&dir, &["commit", "-m", "fixture: initial"])?;
    Ok(dir)
}

/// A remote-clone provision input against the fixture repository.
fn clone_input(fixture: &Path, brief_id: &str, run_id: &str) -> ProvisionInput {
    ProvisionInput {
        repo_root: fixture.to_string_lossy().into_owned(),
        brief_id: brief_id.to_owned(),
        base_ref: "main".to_owned(),
        placement: Placement::Remote,
        isolation: Isolation::Copy,
        clone_url: Some(fixture.to_string_lossy().into_owned()),
        run_id: Some(run_id.to_owned()),
    }
}

/// Whether `path` lives under a volatile root a reboot or temp-reaper
/// purges.
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

/// #175 loss repro: (a) provision with a clone URL, (b) commit a dev round
/// in the workspace, (c) simulate the reboot purge of volatile temp roots,
/// (d) run a later activity step against the recorded `workspace.path` —
/// the step must succeed and the dev-round commit must be intact.
#[test]
fn workspace_survives_reboot_purge_and_later_activity_succeeds() -> TestResult {
    let root = stable_root();
    let fixture = fixture_repo("survival")?;
    let shell = Shell::inherited();

    // (a) provision with clone_url.
    let workspace =
        handlers::provision_workspace(&shell, clone_input(&fixture, "loss-repro", "run-survival"))
            .map_err(|failure| format!("provision failed: {}", failure.message()))?;
    let workspace_dir = PathBuf::from(&workspace.path);

    // (b) a simulated dev round: commit real, unpushed work.
    git(&workspace_dir, &["config", "user.email", "loss-repro@test"])?;
    git(&workspace_dir, &["config", "user.name", "Loss Repro"])?;
    std::fs::write(
        workspace_dir.join("DEV-ROUND.md"),
        "unpushed dev-round work\n",
    )?;
    git(&workspace_dir, &["add", "-A"])?;
    git(
        &workspace_dir,
        &["commit", "-m", "dev round: unpushed work"],
    )?;

    // (c) the reboot: volatile temp roots are purged; durable state (the
    // workflow history carrying `workspace.path`) survives.
    purge_if_volatile(&workspace_dir)?;

    // (d) a later activity step re-dispatched against the recorded path.
    let startup = handlers::startup_task(
        &shell,
        StartupTask::WarmBuild {
            workspace: workspace.clone(),
        },
    )
    .map_err(|failure| {
        format!(
            "later activity failed against the recorded workspace path — \
             the reboot purge lost the clone and its dev-round commit: {}",
            failure.message()
        )
    })?;
    match startup {
        StartupResult::Warmed { build_warm } => assert!(
            build_warm.ok,
            "warm build must succeed in the surviving workspace"
        ),
        StartupResult::Developed { .. } => {
            return Err("warm_build task answered the dev variant".into());
        }
    }

    // The dev-round commit is intact.
    let log = std::process::Command::new("git")
        .args(["log", "--format=%s"])
        .current_dir(&workspace_dir)
        .output()?;
    let subjects = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(
        subjects.contains("dev round: unpushed work"),
        "dev-round commit must survive the reboot; got log: {subjects}"
    );

    // The recorded path must live under the stable root — never under a
    // volatile temp root a reboot would purge.
    assert!(
        workspace_dir.starts_with(root),
        "workspace {} must live under the stable root {}",
        workspace.path,
        root.display()
    );
    Ok(())
}

/// Remote provisioning clones into `<root>/<run id>/repo` under the stable
/// workspace root — never a volatile temp root — with the branch created.
#[test]
fn provision_clones_into_the_stable_per_run_directory() -> TestResult {
    let root = stable_root();
    let fixture = fixture_repo("provision")?;

    let workspace = handlers::provision_workspace(
        &Shell::inherited(),
        clone_input(&fixture, "brief-9", "run-provision"),
    )
    .map_err(|failure| format!("provision failed: {}", failure.message()))?;

    let expected = root.join("run-provision").join("repo");
    assert_eq!(PathBuf::from(&workspace.path), expected);
    assert!(
        !is_under_volatile_root(&expected),
        "workspace {} must not live under a volatile temp root",
        workspace.path
    );
    assert_eq!(workspace.branch, "stacked-dev-brief-9");
    assert!(expected.join(".git").is_dir(), "the clone must be real");
    Ok(())
}

/// A colliding run_id-keyed directory is this execution's own earlier
/// partial provision attempt (run ids are unique per execution; recorded
/// successes are never re-executed on reopen). Re-provisioning renames the
/// stale attempt aside — its contents survive intact, nothing is deleted —
/// and proceeds fresh, so a worker killed mid-clone recovers through reopen
/// with the SAME run id instead of wedging terminally. (The old `/tmp`
/// scheme's exhaustion fallback `rm -rf`'d a survivor.)
#[test]
fn run_id_collision_renames_the_stale_attempt_aside_and_reprovisions() -> TestResult {
    let root = stable_root();
    let fixture = fixture_repo("collision")?;

    let stale = root.join("run-collision");
    std::fs::create_dir_all(&stale)?;
    let sentinel = stale.join("partial-clone.txt");
    std::fs::write(&sentinel, "half a clone from the killed attempt\n")?;

    let workspace = handlers::provision_workspace(
        &Shell::inherited(),
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

/// A payload without a run id (an older workflow bundle) gets a unique
/// per-attempt directory: re-dispatches of the same brief never collide
/// with — and never reuse, rename, or delete — a surviving `brief-<id>`
/// directory kept for salvage.
#[test]
fn fallback_without_run_id_never_collides_with_a_salvaged_directory() -> TestResult {
    let root = stable_root();
    let fixture = fixture_repo("fallback")?;
    let shell = Shell::inherited();

    let salvaged = root.join("brief-fallback");
    std::fs::create_dir_all(&salvaged)?;
    let sentinel = salvaged.join("unpushed-work.txt");
    std::fs::write(&sentinel, "another run's commits live here\n")?;

    let mut input = clone_input(&fixture, "fallback", "unused");
    input.run_id = None;
    let first = handlers::provision_workspace(&shell, input.clone())
        .map_err(|failure| format!("first fallback provision failed: {}", failure.message()))?;
    let second = handlers::provision_workspace(&shell, input)
        .map_err(|failure| format!("second fallback provision failed: {}", failure.message()))?;

    assert_ne!(
        first.path, second.path,
        "each run_id-less attempt must claim its own directory"
    );
    for workspace in [&first, &second] {
        let path = PathBuf::from(&workspace.path);
        assert!(path.starts_with(root), "workspace must live under the root");
        assert!(
            path.parent()
                .and_then(|parent| parent.file_name())
                .is_some_and(|name| name.to_string_lossy().starts_with("brief-fallback-")),
            "fallback directories are keyed by brief id plus a unique suffix; got {}",
            workspace.path
        );
    }
    assert!(
        sentinel.exists(),
        "the salvaged directory must never be reused, renamed, or deleted"
    );
    Ok(())
}

/// A run key that is not a single normal path component is refused before
/// it is joined under the workspace root — `Path::join` and the teardown
/// containment check are lexical, so a `/` or `..` in the key would address
/// paths outside the root.
#[test]
fn provision_rejects_a_run_key_that_escapes_the_root() -> TestResult {
    let _ = stable_root();
    let fixture = fixture_repo("traversal")?;
    let shell = Shell::inherited();

    for bad_run_id in ["../escape", "a/b", "/abs", "..", "."] {
        let failure =
            handlers::provision_workspace(&shell, clone_input(&fixture, "traversal", bad_run_id))
                .err()
                .map(|failure| failure.message().to_owned())
                .unwrap_or_default();
        assert!(
            failure.contains("not a single path component"),
            "run_id {bad_run_id:?} must be refused; got: {failure}"
        );
    }

    // The brief-id fallback key is operator input and gets the same guard.
    let mut input = clone_input(&fixture, "../traversal", "unused");
    input.run_id = None;
    let failure = handlers::provision_workspace(&shell, input)
        .err()
        .map(|failure| failure.message().to_owned())
        .unwrap_or_default();
    assert!(
        failure.contains("not a single path component"),
        "a traversal brief id must be refused; got: {failure}"
    );
    Ok(())
}

/// Teardown deletes the per-run parent when — and only when — it sits under
/// the resolved workspace root.
#[test]
fn teardown_removes_the_per_run_directory_under_the_root() -> TestResult {
    let root = stable_root();
    let repo = root.join("run-teardown").join("repo");
    std::fs::create_dir_all(&repo)?;

    let torn = handlers::teardown_workspace(
        &Shell::inherited(),
        TeardownInput {
            workspace: Workspace {
                path: repo.to_string_lossy().into_owned(),
                branch: "stacked-dev-teardown".to_owned(),
                placement: Placement::Remote,
                isolation: Isolation::Copy,
            },
            repo_root: ".".to_owned(),
        },
    )
    .map_err(|failure| format!("teardown failed: {}", failure.message()))?;

    assert!(torn.cleaned, "teardown must report the removal");
    assert!(
        !root.join("run-teardown").exists(),
        "the per-run parent must be removed"
    );
    assert!(root.exists(), "the workspace root itself must survive");
    Ok(())
}

/// The success-path teardown also sweeps this run's own
/// `<run id>.superseded-<unique>` siblings — the renamed-aside partial
/// provision attempts from crash recovery — while leaving every OTHER
/// run's directory (including other runs' superseded attempts) untouched.
/// Without the sweep the renamed attempts would leak forever: nothing else
/// ever revisits them.
#[test]
fn teardown_sweeps_this_runs_superseded_siblings_only() -> TestResult {
    let root = stable_root();
    let repo = root.join("run-sweep").join("repo");
    std::fs::create_dir_all(&repo)?;
    std::fs::create_dir_all(root.join("run-sweep.superseded-11-p1"))?;
    std::fs::create_dir_all(root.join("run-sweep.superseded-22-p2"))?;
    // Bystanders that must survive: another live run and ITS superseded
    // attempt.
    std::fs::create_dir_all(root.join("run-other").join("repo"))?;
    std::fs::create_dir_all(root.join("run-other.superseded-33-p3"))?;

    let torn = handlers::teardown_workspace(
        &Shell::inherited(),
        TeardownInput {
            workspace: Workspace {
                path: repo.to_string_lossy().into_owned(),
                branch: "stacked-dev-sweep".to_owned(),
                placement: Placement::Remote,
                isolation: Isolation::Copy,
            },
            repo_root: ".".to_owned(),
        },
    )
    .map_err(|failure| format!("teardown failed: {}", failure.message()))?;

    assert!(torn.cleaned, "teardown must report the removal");
    assert!(
        !root.join("run-sweep").exists(),
        "the per-run parent must be removed"
    );
    assert!(
        !root.join("run-sweep.superseded-11-p1").exists()
            && !root.join("run-sweep.superseded-22-p2").exists(),
        "this run's superseded partial attempts must be swept"
    );
    assert!(
        root.join("run-other").join("repo").exists(),
        "another run's directory must survive the sweep"
    );
    assert!(
        root.join("run-other.superseded-33-p3").exists(),
        "another run's superseded attempt must survive the sweep"
    );
    std::fs::remove_dir_all(root.join("run-other"))?;
    std::fs::remove_dir_all(root.join("run-other.superseded-33-p3"))?;
    Ok(())
}

/// Teardown refuses to delete a remote workspace whose parent is outside
/// the resolved workspace root — no parent heuristics, ever.
#[test]
fn teardown_refuses_paths_outside_the_workspace_root() -> TestResult {
    let _ = stable_root();
    let outside = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("outside-root-{}", std::process::id()));
    let repo = outside.join("repo");
    std::fs::create_dir_all(&repo)?;

    let torn = handlers::teardown_workspace(
        &Shell::inherited(),
        TeardownInput {
            workspace: Workspace {
                path: repo.to_string_lossy().into_owned(),
                branch: "stacked-dev-outside".to_owned(),
                placement: Placement::Remote,
                isolation: Isolation::Copy,
            },
            repo_root: ".".to_owned(),
        },
    )
    .map_err(|failure| format!("teardown failed: {}", failure.message()))?;

    assert!(
        !torn.cleaned,
        "teardown must refuse a workspace outside the root"
    );
    assert!(
        repo.exists(),
        "a workspace outside the root must never be deleted"
    );
    std::fs::remove_dir_all(&outside)?;
    Ok(())
}

/// A recorded workspace path whose parent LEXICALLY starts with the root
/// but traverses out of it via `..` is refused: the ownership guard
/// canonicalizes both sides before the containment check, while a bare
/// `Path::starts_with` would pass it and `remove_dir_all` would resolve the
/// `..` at the filesystem level.
#[test]
fn teardown_refuses_a_parent_that_escapes_the_root_via_dot_dot() -> TestResult {
    let root = stable_root();
    let victim = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("teardown-victim-{}", std::process::id()));
    std::fs::create_dir_all(victim.join("repo"))?;
    std::fs::create_dir_all(root.join("x"))?;

    // `<root>/x/../../teardown-victim-<pid>` — lexically under the root,
    // physically the sibling victim directory outside it.
    let escaping_parent = root
        .join("x")
        .join("..")
        .join("..")
        .join(victim.file_name().ok_or("victim has a name")?);
    assert!(
        escaping_parent.starts_with(root),
        "precondition: the escaping parent must pass the LEXICAL check"
    );

    let torn = handlers::teardown_workspace(
        &Shell::inherited(),
        TeardownInput {
            workspace: Workspace {
                path: escaping_parent.join("repo").to_string_lossy().into_owned(),
                branch: "stacked-dev-escape".to_owned(),
                placement: Placement::Remote,
                isolation: Isolation::Copy,
            },
            repo_root: ".".to_owned(),
        },
    )
    .map_err(|failure| format!("teardown failed: {}", failure.message()))?;

    assert!(
        !torn.cleaned,
        "teardown must refuse a `..`-escaping workspace parent"
    );
    assert!(
        victim.join("repo").exists(),
        "the escape target must never be deleted"
    );
    std::fs::remove_dir_all(&victim)?;
    Ok(())
}

/// An activity dispatched against a genuinely missing workspace fails with
/// the explicit missing-workspace diagnostic.
#[test]
fn missing_workspace_fails_with_the_explicit_diagnostic() {
    let root = stable_root();
    let dead = root.join("run-never-provisioned").join("repo");

    let failure = handlers::scout(
        &Shell::inherited(),
        ScoutInput {
            workspace: Workspace {
                path: dead.to_string_lossy().into_owned(),
                branch: "stacked-dev-dead".to_owned(),
                placement: Placement::Remote,
                isolation: Isolation::Copy,
            },
            prompt: "orient".to_owned(),
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
