//! Plain activity handler bodies for the `agent_dev` proof: `provision`,
//! `gate`, and `land`.
//!
//! Each handler shells to the real CLI that owns the step (`git` for the
//! clone/branch/commit, `cargo` for the gate) through [`crate::shell::Shell`].
//! Failure classification: a CLI that cannot RUN at all (missing executable,
//! dead working directory) and a command the contract requires to exit zero
//! are **terminal** activity failures — retrying a broken environment cannot
//! help. A non-zero gate exit is recorded data (`pass: false` with the
//! diagnostics tail), never an error.
//!
//! Workspace discipline is the #175 seam reused from the stacked-dev-remote
//! worker: the clone lives at `<root>/<run_id>/repo` under the STABLE
//! workspace root (`AION_WORKSPACE_ROOT`, default `~/.aion/clones` — never
//! the OS temp dir: the path is recorded in durable workflow history and must
//! survive a host reboot), a colliding run directory is renamed aside — never
//! deleted — and the run key is refused unless it is a single normal path
//! component.
//!
//! The functions are plain synchronous `(&Shell, Input) -> Result<Output, _>`
//! (provision also takes the resolved workspace root, threaded ONCE from the
//! composition root) so the hermetic tests drive them directly with fake-CLI
//! shims on a private `PATH`; `main.rs` adapts them onto the worker's async
//! handler signature via `spawn_blocking`.

use std::path::{Path, PathBuf};

use aion_worker::ActivityFailure;

use crate::shell::{CliRun, Shell};
use crate::types::{
    AssistantProvisionInput, AssistantWorkspace, GateInput, GateResult, LandInput, LandResult,
    ProvisionInput, Workspace,
};

/// Environment variable overriding the stable workspace root for clones.
pub const WORKSPACE_ROOT_ENV: &str = "AION_WORKSPACE_ROOT";

/// Default workspace root, relative to `$HOME`.
const DEFAULT_WORKSPACE_ROOT: &str = ".aion/clones";

/// How much of a failing gate command's combined output rides in the recorded
/// diagnostics. Presentational truncation only — the TAIL is kept because
/// cargo prints the decisive errors and the failure summary last.
const DIAGNOSTICS_TAIL: usize = 8000;

/// The two gate commands, in order: clippy (deny warnings) then the test
/// suite, both workspace-wide in the clone.
const GATE_COMMANDS: [(&str, &[&str]); 2] = [
    (
        "cargo clippy",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ),
    ("cargo test", &["test", "--workspace"]),
];

/// Resolve the stable workspace root for clones: `AION_WORKSPACE_ROOT` when
/// set, else `~/.aion/clones`. NEVER the OS temp directory — the clone path
/// is recorded in durable workflow history and must survive a host reboot
/// (#175). Called ONCE at the composition root and threaded to both the
/// provision handler and the agent harness's `--workspace-root` template.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when neither source yields an absolute path
/// (see [`workspace_root_from`]).
pub fn resolve_workspace_root() -> Result<PathBuf, ActivityFailure> {
    workspace_root_from(
        std::env::var_os(WORKSPACE_ROOT_ENV),
        std::env::var_os("HOME"),
    )
}

/// Pure resolution seam behind [`resolve_workspace_root`]: the override wins
/// when non-empty, else `$HOME/.aion/clones`; neither is a hard, clear
/// terminal error — never a silent temp-dir fallback. The resolved root must
/// be ABSOLUTE: a relative root resolves against the worker process CWD at
/// every call site, so the same recorded history would name a different
/// directory after a worker restarted from elsewhere.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when both the override and `HOME` are unset
/// or empty, or when the resolved root is not an absolute path.
pub fn workspace_root_from(
    override_root: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, ActivityFailure> {
    if let Some(root) = override_root
        && !root.is_empty()
    {
        return require_absolute(PathBuf::from(root), WORKSPACE_ROOT_ENV);
    }
    match home {
        Some(home) if !home.is_empty() => {
            require_absolute(PathBuf::from(home).join(DEFAULT_WORKSPACE_ROOT), "HOME")
        }
        _ => Err(ActivityFailure::terminal(format!(
            "cannot resolve a stable workspace root: HOME is unset and \
             {WORKSPACE_ROOT_ENV} is not set — set {WORKSPACE_ROOT_ENV} to a \
             durable directory (never a temp dir: the workspace path is \
             recorded in durable workflow history and must survive a reboot)"
        ))),
    }
}

/// Require the resolved workspace root to be an absolute path — relative
/// roots are CWD-dependent and change meaning across worker restarts (see
/// [`workspace_root_from`]).
fn require_absolute(root: PathBuf, source_name: &str) -> Result<PathBuf, ActivityFailure> {
    if root.is_absolute() {
        Ok(root)
    } else {
        Err(ActivityFailure::terminal(format!(
            "workspace root {} (from {source_name}) is not an absolute path — \
             a relative root resolves against the worker's current directory \
             and names a different location after a restart from elsewhere; \
             set {WORKSPACE_ROOT_ENV} to an absolute, durable directory",
            root.display()
        )))
    }
}

/// Require the workspace directory to exist before dispatching work into it.
/// A missing directory means the worker host no longer has the workspace (a
/// reboot, temp-reaper, or manual cleanup) while the durable history still
/// names it: the run cannot make progress and must fail loudly — retrying a
/// dead path cannot help (#175).
fn require_workspace_dir(path: &str) -> Result<(), ActivityFailure> {
    if Path::new(path).is_dir() {
        Ok(())
    } else {
        Err(ActivityFailure::terminal(format!(
            "workspace missing at {path} — the worker host no longer has it \
             (lost clone); run cannot resume"
        )))
    }
}

/// Require `key` to be exactly one normal path component before it is joined
/// under the workspace root. Run ids are engine-minted workflow ids in
/// practice, but the key arrives over the wire from durable history and
/// `Path::join` is lexical — a key carrying `/`, `\`, or `..` would address
/// paths outside the root.
fn require_single_component(key: &str, what: &str) -> Result<(), ActivityFailure> {
    let mut components = Path::new(key).components();
    let single_normal = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if single_normal && !key.contains('\\') {
        Ok(())
    } else {
        Err(ActivityFailure::terminal(format!(
            "{what} {key:?} is not a single path component — refusing to key \
             a workspace directory under the workspace root with it"
        )))
    }
}

/// `provision`: clone `repo_url` into `<root>/<run_id>/repo` and create the
/// working branch `agent-dev-<brief_id>` off `base_ref` there.
///
/// The workspace path is recorded in durable workflow history and every later
/// activity — including after a server, worker, or HOST restart — dispatches
/// against it, so it lives under the stable `root` resolved once at the
/// composition root (#175). A colliding run directory is this execution's own
/// earlier partial provision attempt (engine-minted run ids are unique per
/// execution and a recorded provision success is never re-executed): it is
/// renamed aside — never deleted — and provisioning proceeds fresh (see
/// [`claim_run_directory`]).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the run id is not a single path
/// component, when the workspace directory cannot be claimed, or when
/// `git clone` / `git checkout -b` cannot run or exits non-zero.
pub fn provision(
    shell: &Shell,
    root: &Path,
    input: ProvisionInput,
) -> Result<Workspace, ActivityFailure> {
    let ProvisionInput {
        repo_url,
        base_ref,
        brief_id,
        run_id,
    } = input;
    std::fs::create_dir_all(root).map_err(|source| {
        ActivityFailure::terminal(format!(
            "cannot create workspace root {}: {source}",
            root.display()
        ))
    })?;
    let run_dir = claim_run_directory(root, &run_id)?;
    let run_dir_str = run_dir.to_string_lossy().into_owned();
    let repo_dir = run_dir.join("repo").to_string_lossy().into_owned();

    require_run(
        shell,
        "git",
        &["clone", &repo_url, &repo_dir],
        &run_dir_str,
        "git clone",
    )?;

    let branch = format!("agent-dev-{brief_id}");
    require_run(
        shell,
        "git",
        &["checkout", "-b", &branch, &base_ref],
        &repo_dir,
        "git checkout -b",
    )?;

    Ok(Workspace {
        path: repo_dir,
        branch,
    })
}

/// `assistant_provision` (the `assistant` workflow package): materialise the
/// session workspace at `<root>/<run_id>/repo` — `git clone` of `repo_path`
/// when given (a local path or URL), a fresh `git init` scratch workspace
/// when empty, so the assistant always has a real, versionable working
/// directory for the norn harness's `--workspace-root`/`-C` template.
///
/// Same #175 discipline as [`provision`]: the path is recorded in durable
/// workflow history, so it lives under the stable resolved `root`, keyed by
/// the engine-minted run id, and a colliding run directory (this execution's
/// own earlier partial attempt) is renamed aside — never deleted.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the run id is not a single path
/// component, when the workspace directory cannot be claimed, or when the
/// `git clone` / `git init` cannot run or exits non-zero.
pub fn assistant_provision(
    shell: &Shell,
    root: &Path,
    input: AssistantProvisionInput,
) -> Result<AssistantWorkspace, ActivityFailure> {
    let AssistantProvisionInput { repo_path, run_id } = input;
    std::fs::create_dir_all(root).map_err(|source| {
        ActivityFailure::terminal(format!(
            "cannot create workspace root {}: {source}",
            root.display()
        ))
    })?;
    let run_dir = claim_run_directory(root, &run_id)?;
    let run_dir_str = run_dir.to_string_lossy().into_owned();
    let repo_dir = run_dir.join("repo").to_string_lossy().into_owned();

    if repo_path.is_empty() {
        std::fs::create_dir(&repo_dir).map_err(|source| {
            ActivityFailure::terminal(format!(
                "cannot create scratch workspace directory {repo_dir}: {source}"
            ))
        })?;
        require_run(shell, "git", &["init"], &repo_dir, "git init")?;
    } else {
        require_run(
            shell,
            "git",
            &["clone", &repo_path, &repo_dir],
            &run_dir_str,
            "git clone",
        )?;
    }
    materialize_skill_resources(Path::new(&repo_dir))?;

    Ok(AssistantWorkspace { path: repo_dir })
}

/// The assistant's skill documents, embedded at compile time so every session
/// gets them regardless of whether an aion repository is present. They are
/// the assistant's primary operating manual; the repository is optional
/// enrichment on top.
const SKILL_RESOURCES: [(&str, &str); 5] = [
    (
        "ENVIRONMENT.md",
        include_str!("../../../assistant/resources/ENVIRONMENT.md"),
    ),
    (
        "SCAFFOLD.md",
        include_str!("../../../assistant/resources/SCAFFOLD.md"),
    ),
    (
        "COMMANDS.md",
        include_str!("../../../assistant/resources/COMMANDS.md"),
    ),
    (
        "SDK.md",
        include_str!("../../../assistant/resources/SDK.md"),
    ),
    (
        "TROUBLESHOOTING.md",
        include_str!("../../../assistant/resources/TROUBLESHOOTING.md"),
    ),
];

/// Write the embedded skill documents into `<workspace>/.assistant/resources/`
/// — inside the workspace so the harness's confined file tools can read them,
/// dot-prefixed so they stay out of the way of the operator's actual work
/// (and untracked-but-ignorable inside a repo clone).
fn materialize_skill_resources(workspace: &Path) -> Result<(), ActivityFailure> {
    let resources = workspace.join(".assistant").join("resources");
    std::fs::create_dir_all(&resources).map_err(|source| {
        ActivityFailure::terminal(format!(
            "cannot create skill-resource directory {}: {source}",
            resources.display()
        ))
    })?;
    for (name, contents) in SKILL_RESOURCES {
        std::fs::write(resources.join(name), contents).map_err(|source| {
            ActivityFailure::terminal(format!(
                "cannot write skill resource {name} into {}: {source}",
                resources.display()
            ))
        })?;
    }
    Ok(())
}

/// Claim `<root>/<run_id>` for this provision attempt. A collision is this
/// execution's own earlier partial attempt (see [`provision`]): the stale
/// directory is renamed aside to `<run_id>.superseded-<unique>` — never
/// deleted — and the claim proceeds fresh, so a worker killed mid-clone stays
/// recoverable through reopen (which re-executes provision with the SAME id)
/// instead of wedging terminally, and a lost-but-still-writing worker keeps
/// writing into the moved directory instead of racing the new clone.
fn claim_run_directory(root: &Path, run_id: &str) -> Result<PathBuf, ActivityFailure> {
    require_single_component(run_id, "run_id")?;
    let run_dir = root.join(run_id);
    match std::fs::create_dir(&run_dir) {
        Ok(()) => Ok(run_dir),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            let stale = root.join(format!("{run_id}.superseded-{}", unique_suffix()));
            std::fs::rename(&run_dir, &stale).map_err(|source| {
                ActivityFailure::terminal(format!(
                    "cannot move the stale provision attempt {} aside to {}: {source}",
                    run_dir.display(),
                    stale.display()
                ))
            })?;
            std::fs::create_dir(&run_dir).map_err(|source| {
                ActivityFailure::terminal(format!(
                    "cannot create workspace directory {}: {source}",
                    run_dir.display()
                ))
            })?;
            Ok(run_dir)
        }
        Err(source) => Err(ActivityFailure::terminal(format!(
            "cannot create workspace directory {}: {source}",
            run_dir.display()
        ))),
    }
}

/// A per-attempt unique suffix for workspace directory names: wall-clock
/// nanoseconds plus the worker pid. Not a security token — just enough that
/// two provision attempts never mint the same directory name.
fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-p{}", std::process::id())
}

/// `gate`: run `cargo clippy --workspace --all-targets -- -D warnings` then
/// `cargo test --workspace` in the workspace. A command that RAN and exited
/// non-zero is the recorded `pass: false` verdict carrying the combined
/// output's tail (the later command does not run — its verdict could not
/// flip the recorded failure); both exiting zero is `pass: true`.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the workspace is missing or when a gate
/// command cannot RUN at all (missing `cargo`, dead working directory) —
/// that is a broken environment, not a gate verdict.
pub fn gate(shell: &Shell, input: GateInput) -> Result<GateResult, ActivityFailure> {
    let GateInput { path, branch: _ } = input;
    require_workspace_dir(&path)?;
    for (context, args) in GATE_COMMANDS {
        let command_run = shell.run("cargo", args, &path).map_err(|failure| {
            ActivityFailure::terminal(format!("{context}: {}", failure.message()))
        })?;
        if !command_run.succeeded() {
            return Ok(GateResult {
                pass: false,
                diagnostics: format!(
                    "{context} failed — exit status {}:\n{}",
                    command_run.exit_status,
                    tail(&command_run.output, DIAGNOSTICS_TAIL)
                ),
            });
        }
    }
    Ok(GateResult {
        pass: true,
        diagnostics: String::new(),
    })
}

/// `land`: commit the run's work (`git add -A` + `git commit`) in the clone
/// and return the created commit's SHA.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the workspace is missing, when `git`
/// cannot run, or when the add/commit/rev-parse exits non-zero (a commit with
/// nothing to commit is a terminal failure — the workflow must not record a
/// phantom landing).
pub fn land(shell: &Shell, input: LandInput) -> Result<LandResult, ActivityFailure> {
    let LandInput {
        workspace,
        brief_id,
    } = input;
    let path = workspace.path;
    require_workspace_dir(&path)?;
    require_run(shell, "git", &["add", "-A"], &path, "git add")?;
    let message = format!("agent-dev: {brief_id}");
    require_run(
        shell,
        "git",
        &["commit", "-m", &message],
        &path,
        "git commit",
    )?;
    let rev_parse = require_run(shell, "git", &["rev-parse", "HEAD"], &path, "git rev-parse")?;
    let commit_sha = rev_parse.stdout.trim().to_owned();
    if commit_sha.is_empty() {
        return Err(ActivityFailure::terminal(
            "git rev-parse HEAD printed no commit sha after the land commit".to_owned(),
        ));
    }
    Ok(LandResult { commit_sha })
}

/// Require a command to run AND exit zero; anything else is a terminal
/// activity failure carrying the command's diagnostics.
fn require_run(
    shell: &Shell,
    executable: &str,
    args: &[&str],
    cwd: &str,
    context: &str,
) -> Result<CliRun, ActivityFailure> {
    match shell.run(executable, args, cwd) {
        Ok(command_run) if command_run.succeeded() => Ok(command_run),
        Ok(command_run) => Err(ActivityFailure::terminal(format!(
            "{context} failed — exit status {}: {}",
            command_run.exit_status,
            command_run.output.trim()
        ))),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "{context}: {}",
            failure.message()
        ))),
    }
}

/// Last `limit` characters of `text`, truncated on a char boundary — the
/// TAIL, because cargo prints the decisive errors and failure summary last.
fn tail(text: &str, limit: usize) -> &str {
    let count = text.chars().count();
    match count.checked_sub(limit) {
        Some(skip) if skip > 0 => {
            let boundary = text
                .char_indices()
                .nth(skip)
                .map_or(text.len(), |(index, _)| index);
            &text[boundary..]
        }
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{tail, workspace_root_from};

    #[test]
    fn workspace_root_override_wins_over_home() {
        let resolved = workspace_root_from(
            Some(OsString::from("/durable/clones")),
            Some(OsString::from("/home/user")),
        );
        assert_eq!(resolved.ok(), Some(PathBuf::from("/durable/clones")));
    }

    #[test]
    fn workspace_root_defaults_under_home() {
        let resolved = workspace_root_from(None, Some(OsString::from("/home/user")));
        assert_eq!(
            resolved.ok(),
            Some(PathBuf::from("/home/user/.aion/clones"))
        );
    }

    #[test]
    fn workspace_root_empty_override_falls_back_to_home() {
        let resolved =
            workspace_root_from(Some(OsString::new()), Some(OsString::from("/home/user")));
        assert_eq!(
            resolved.ok(),
            Some(PathBuf::from("/home/user/.aion/clones"))
        );
    }

    #[test]
    fn workspace_root_without_home_or_override_is_a_hard_error() {
        let message = workspace_root_from(None, None)
            .err()
            .map(|failure| failure.message().to_owned())
            .unwrap_or_default();
        assert!(
            message.contains("AION_WORKSPACE_ROOT") && message.contains("HOME"),
            "the error must name both the override and HOME; got: {message}"
        );
    }

    #[test]
    fn workspace_root_relative_override_is_a_hard_error() {
        // A relative root resolves against the worker CWD at each call site,
        // so the recorded history's path silently changes meaning after a
        // restart from a different directory — refuse it loudly.
        let message = workspace_root_from(
            Some(OsString::from("relative/clones")),
            Some(OsString::from("/home/user")),
        )
        .err()
        .map(|failure| failure.message().to_owned())
        .unwrap_or_default();
        assert!(
            message.contains("absolute") && message.contains("relative/clones"),
            "the error must name the offending path and demand an absolute one; got: {message}"
        );
    }

    #[test]
    fn workspace_root_relative_home_is_a_hard_error() {
        let message = workspace_root_from(None, Some(OsString::from("relative-home")))
            .err()
            .map(|failure| failure.message().to_owned())
            .unwrap_or_default();
        assert!(
            message.contains("absolute"),
            "a relative HOME-derived root must be refused; got: {message}"
        );
    }

    #[test]
    fn tail_keeps_the_end_of_long_output_and_short_output_whole() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("abcdefgh", 3), "fgh");
        // Multi-byte chars truncate on a char boundary, never mid-codepoint.
        assert_eq!(tail("ééééé", 2), "éé");
    }
}
