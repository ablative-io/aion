//! Activity handler bodies, mirroring `../src/stacked_dev/locals.gleam`
//! invocation for invocation.
//!
//! Each handler shells to the real CLI that owns the step (`yg` for worktree
//! provisioning, affected-module scoping, and diagnostics checks, `norn` for
//! the dev agent, `cargo` for the advisory warm build, `meridian` for review
//! requests and landing) through [`crate::shell::Shell`]. Failure
//! classification follows the local implementations: a CLI that cannot run
//! (missing executable, dead working directory) and a command the contract
//! requires to exit zero are **terminal** activity failures — retrying a
//! broken environment cannot help. A non-zero exit that the contract treats
//! as recorded data (a forfeited warm cache, check diagnostics, a gate
//! report) is a successful activity result, never an error.
//!
//! The functions are plain synchronous `(Shell, Input) -> Result<Output, _>`
//! so the hermetic tests drive them directly with fake-CLI shims on a
//! private `PATH`; `main.rs` adapts them onto the worker's async handler
//! signature.

use std::path::PathBuf;

use aion_worker::ActivityFailure;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::schemas::{DEV_OUTPUT_SCHEMA, REVIEW_OUTPUT_SCHEMA, SCOUT_OUTPUT_SCHEMA};
use crate::shell::{CliRun, Shell};
use crate::types::{
    AssembleInput, AssembledWave, BriefDocument, CheckResult, CheckVerdict, DevInput, DevReport,
    EnrichInput, GateInput, GateResult, GateScope, GateVerdict, Isolation, LandInput, Landed,
    Placement, ProvisionInput, ResumeInput, ReviewAck, ReviewInput, ReviewReport, ReviewRequest,
    ScopedInput, ScoutInput, ScoutReport, StartupResult, StartupTask, TeardownInput, TornDown,
    Workspace,
};

/// How much of an unparseable norn stdout rides in the terminal failure
/// message. Presentational truncation only: enough to diagnose the envelope
/// shape without shipping megabytes of agent transcript through the failure
/// payload.
const UNPARSEABLE_OUTPUT_HEAD: usize = 1000;

/// Environment variable overriding the stable workspace root for remote
/// clones.
pub const WORKSPACE_ROOT_ENV: &str = "AION_WORKSPACE_ROOT";

/// Default workspace root, relative to `$HOME`.
const DEFAULT_WORKSPACE_ROOT: &str = ".aion/clones";

/// Resolve the stable workspace root for remote clones:
/// `AION_WORKSPACE_ROOT` when set, else `~/.aion/clones`. NEVER the OS temp
/// directory — the clone path is recorded in durable workflow history and
/// must survive a host reboot; temp reapers and reboots purge `/tmp` and
/// `/var/folders`, which loses every unpushed dev-round commit when a run
/// resumes (#175).
fn resolve_workspace_root() -> Result<PathBuf, ActivityFailure> {
    workspace_root_from(
        std::env::var_os(WORKSPACE_ROOT_ENV),
        std::env::var_os("HOME"),
    )
}

/// Pure resolution seam behind [`resolve_workspace_root`]: the override
/// wins when non-empty, else `$HOME/.aion/clones`; neither is a hard, clear
/// terminal error — never a silent temp-dir fallback. The resolved root
/// must be ABSOLUTE: a relative root resolves against the worker process
/// CWD at every call site, so the same recorded history would name a
/// different directory after a worker restarted from elsewhere — and the
/// missing-workspace diagnostic would then lie about an intact clone.
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

/// Require the workspace directory to exist before dispatching work into
/// it. A missing directory means the worker host no longer has the
/// workspace (for a remote clone: a reboot, temp-reaper, or manual cleanup;
/// for a local worktree: a removed or pruned worktree) while the durable
/// history still names it: the run cannot make progress and must fail
/// loudly — retrying a dead path cannot help (#175).
fn require_workspace_dir(path: &str) -> Result<(), ActivityFailure> {
    if std::path::Path::new(path).is_dir() {
        Ok(())
    } else {
        Err(ActivityFailure::terminal(format!(
            "workspace missing at {path} — the worker host no longer has it \
             (lost clone or removed worktree); run cannot resume"
        )))
    }
}

/// Require `key` to be exactly one normal path component before it is
/// joined under the workspace root. Run ids are engine UUIDs in practice,
/// but the key arrives over the wire from durable history (and the brief-id
/// fallback is operator input), and both `Path::join` and the teardown
/// containment check are lexical — a key carrying `/`, `\`, or `..` would
/// address (and later delete) paths outside the root.
fn require_single_component(key: &str, what: &str) -> Result<(), ActivityFailure> {
    let mut components = std::path::Path::new(key).components();
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

/// `provision_workspace`: provision an isolated workspace.
///
/// When `clone_url` is present, clones the repo into a per-run directory
/// under the stable workspace root (`AION_WORKSPACE_ROOT`, default
/// `~/.aion/clones` — never the OS temp dir, #175) and creates the branch
/// there — no `yg` needed (the remote machine has no yggdrasil tree). When
/// absent, falls back to `yg` worktree provisioning.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `git`/`yg` cannot run, when the clone
/// fails, or when the isolation mode has no implementation.
pub fn provision_workspace(
    shell: &Shell,
    input: ProvisionInput,
) -> Result<Workspace, ActivityFailure> {
    if input.clone_url.is_some() {
        provision_clone(shell, &input)
    } else {
        match input.isolation {
            Isolation::Worktree => provision_worktree(shell, input),
            Isolation::Copy | Isolation::Overlay | Isolation::Vm => {
                Err(ActivityFailure::terminal(format!(
                    "isolation mode {} is a typed seam with no local implementation",
                    input.isolation.wire_name()
                )))
            }
        }
    }
}

/// Clone from a URL into a per-run directory under the stable workspace
/// root, create the working branch.
///
/// The workspace path is recorded in durable workflow history and every
/// later activity — including after a server, worker, or HOST restart —
/// re-dispatches against it, so it must live somewhere a reboot cannot
/// purge (#175): `<root>/<run_id>/repo`, keyed by the workflow execution's
/// unique run id.
///
/// Collision policy — provisioning never DELETES anything:
///
/// - A colliding **run_id-keyed** directory is treated as an earlier
///   partial provision attempt of this same execution: engine-minted run
///   ids are unique per execution and a recorded provision success is never
///   re-executed (a reopen re-dispatches only activities whose last attempt
///   failed). It is renamed aside to `<run_id>.superseded-<unique>` and
///   provisioning proceeds fresh — a worker killed mid-clone stays
///   recoverable through reopen (which re-executes provision with the SAME
///   id) instead of wedging terminally, and a lost-but-still-writing worker
///   keeps writing into the moved directory instead of racing the new
///   clone. CAVEAT: the engine does NOT enforce uniqueness for
///   caller-supplied workflow ids (`Engine::start_workflow_with_id` leaves
///   choosing an unused id to the caller), so a dispatcher that reuses an
///   id makes the new run displace the earlier run's live-or-salvage
///   workspace — the rename keeps the data and the displaced run fails
///   loudly at its next activity (`workspace missing`), but id uniqueness
///   is the dispatcher's responsibility, not a guarantee this code can
///   assume.
/// - When no run id rides on the input (an older workflow bundle), the
///   brief id PLUS a per-attempt unique suffix keys the directory: brief
///   ids recur across dispatches (a failed run deliberately keeps its
///   workspace for salvage), so a bare brief-keyed directory would collide
///   on every re-dispatch. The unique key never reuses, renames, or touches
///   a surviving run's directory.
fn provision_clone(shell: &Shell, input: &ProvisionInput) -> Result<Workspace, ActivityFailure> {
    let clone_url = input.clone_url.as_deref().ok_or_else(|| {
        ActivityFailure::terminal("provision_clone called without clone_url".to_owned())
    })?;

    let branch = format!("stacked-dev-{}", input.brief_id);

    let root = resolve_workspace_root()?;
    std::fs::create_dir_all(&root).map_err(|source| {
        ActivityFailure::terminal(format!(
            "cannot create workspace root {}: {source}",
            root.display()
        ))
    })?;
    let run_dir = match input.run_id.as_deref() {
        Some(run_id) if !run_id.is_empty() => claim_run_directory(&root, run_id)?,
        _ => claim_fallback_directory(&root, &input.brief_id)?,
    };

    let run_dir_str = run_dir.to_string_lossy().into_owned();
    let repo_dir = run_dir.join("repo").to_string_lossy().into_owned();

    require_run(
        shell,
        "git",
        &["clone", clone_url, &repo_dir],
        &run_dir_str,
        "git clone",
    )?;

    // Create the working branch from the base ref.
    require_run(
        shell,
        "git",
        &["checkout", "-b", &branch, &input.base_ref],
        &repo_dir,
        "git checkout -b",
    )?;

    Ok(Workspace {
        path: repo_dir,
        branch,
        placement: Placement::Remote,
        isolation: Isolation::Copy,
    })
}

/// Claim `<root>/<run_id>` for this provision attempt. A collision is this
/// execution's own earlier partial attempt (see [`provision_clone`]): the
/// stale directory is renamed aside — never deleted — and the claim
/// proceeds fresh.
fn claim_run_directory(root: &std::path::Path, run_id: &str) -> Result<PathBuf, ActivityFailure> {
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

/// Claim `<root>/brief-<brief_id>-<unique>` for a payload that carries no
/// run id (an older workflow bundle). The per-attempt unique suffix means a
/// re-dispatch of the same brief never collides with — and never reuses,
/// renames, or deletes — a surviving run's directory kept for salvage.
fn claim_fallback_directory(
    root: &std::path::Path,
    brief_id: &str,
) -> Result<PathBuf, ActivityFailure> {
    let key = format!("brief-{brief_id}-{}", unique_suffix());
    require_single_component(&key, "brief_id-derived workspace key")?;
    let dir = root.join(&key);
    std::fs::create_dir(&dir).map_err(|source| {
        ActivityFailure::terminal(format!(
            "cannot create workspace directory {}: {source}",
            dir.display()
        ))
    })?;
    Ok(dir)
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

/// Local worktree provisioning via `yg` — fallback when no `clone_url`.
fn provision_worktree(shell: &Shell, input: ProvisionInput) -> Result<Workspace, ActivityFailure> {
    let ProvisionInput {
        repo_root,
        brief_id,
        base_ref,
        placement,
        isolation,
        ..
    } = input;
    let base_branch = format!("stacked-dev-{brief_id}");
    let branch = resolve_branch_name(shell, &base_branch, &base_ref, &repo_root)?;
    let worktree_path = format!("{repo_root}/.yggdrasil-worktrees/{branch}");

    require_run(
        shell,
        "yg",
        &["branch", "provision", &branch, "--path", &worktree_path],
        &repo_root,
        "yg branch provision",
    )?;
    Ok(Workspace {
        path: worktree_path,
        branch,
        placement,
        isolation,
    })
}

/// How many branch names `resolve_branch_name` tries before giving up. The
/// bound keeps exhaustion terminal and loud instead of looping forever
/// against a persistently failing `yg`.
const BRANCH_NAME_ATTEMPT_CAP: u32 = 10;

/// Try `yg branch add` with the base name, then `-attempt-2`, `-attempt-3`,
/// etc. until one succeeds. Returns the branch name that was added. A name
/// already taken retries with the next suffix; any other `yg` failure is
/// terminal immediately, and exhausting the bounded attempts is terminal
/// carrying the last run's diagnostics — never a silent guess.
fn resolve_branch_name(
    shell: &Shell,
    base: &str,
    base_ref: &str,
    repo_root: &str,
) -> Result<String, ActivityFailure> {
    let mut last_diagnostics = String::new();
    for attempt in 1..=BRANCH_NAME_ATTEMPT_CAP {
        let candidate = if attempt == 1 {
            base.to_owned()
        } else {
            format!("{base}-attempt-{attempt}")
        };
        match try_branch_add(shell, &candidate, base_ref, repo_root)? {
            BranchAdd::Created => return Ok(candidate),
            BranchAdd::AlreadyExists { diagnostics } => last_diagnostics = diagnostics,
        }
    }
    Err(ActivityFailure::terminal(format!(
        "could not find an unused branch name after {BRANCH_NAME_ATTEMPT_CAP} \
         attempts (base: {base}); last {last_diagnostics}"
    )))
}

/// Outcome of one `yg branch add` attempt.
enum BranchAdd {
    /// The branch was created.
    Created,
    /// The name is already taken; the diagnostics ride along so exhaustion
    /// can surface them.
    AlreadyExists {
        /// The failing run's rendered diagnostics.
        diagnostics: String,
    },
}

/// Attempt `yg branch add`. Success creates the branch; a non-zero exit
/// whose output names an existing branch is the retryable collision; any
/// other failure is terminal with the run's diagnostics.
fn try_branch_add(
    shell: &Shell,
    branch: &str,
    base_ref: &str,
    repo_root: &str,
) -> Result<BranchAdd, ActivityFailure> {
    match shell.run("yg", &["branch", "add", branch, base_ref], repo_root) {
        Ok(run) if run.succeeded() => Ok(BranchAdd::Created),
        Ok(run) if run.output.contains("already exists") => Ok(BranchAdd::AlreadyExists {
            diagnostics: format!(
                "yg branch add failed — exit status {}: {}",
                run.exit_status,
                run.output.trim()
            ),
        }),
        Ok(run) => Err(ActivityFailure::terminal(format!(
            "yg branch add failed — exit status {}: {}",
            run.exit_status,
            run.output.trim()
        ))),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "yg branch add: {}",
            failure.message()
        ))),
    }
}

/// Serve one startup fan-out task: the advisory warm build or the dev round.
/// Registered for BOTH the `warm_build` and `dev` activity names — the two
/// activities flow through one homogeneous `workflow.all` fan-out, so they
/// share the tagged `StartupTask`/`StartupResult` envelope.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the owning CLI cannot run (`cargo` for
/// the warm build, `norn` for the dev round), when `norn` exits non-zero, or
/// when its output matches neither documented `DevResult` shape. A failed
/// `cargo build` is NOT an error — it is the recorded `ok: false` outcome.
pub fn startup_task(shell: &Shell, task: StartupTask) -> Result<StartupResult, ActivityFailure> {
    match task {
        StartupTask::WarmBuild { workspace } => warm_build(shell, &workspace),
        StartupTask::Dev { dev_input } => dev(shell, &dev_input),
    }
}

/// Warm the build cache with `cargo build` in the workspace.
///
/// Advisory by contract: a failed build forfeits the warm cache and is
/// recorded as `ok: false` — it must never fail the run. A missing `cargo`
/// executable is still a loud terminal failure: that is a broken
/// environment, not a forfeited cache.
fn warm_build(shell: &Shell, workspace: &Workspace) -> Result<StartupResult, ActivityFailure> {
    require_workspace_dir(&workspace.path)?;
    match shell.run("cargo", &["build"], &workspace.path) {
        Ok(command_run) => Ok(StartupResult::Warmed {
            build_warm: crate::types::BuildWarm {
                ok: command_run.succeeded(),
                duration_ms: command_run.duration_ms,
            },
        }),
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "cargo build: {}",
            failure.message()
        ))),
    }
}

/// Run the dev agent against the projected dev prompt via the `norn` CLI.
fn dev(shell: &Shell, input: &DevInput) -> Result<StartupResult, ActivityFailure> {
    require_workspace_dir(&input.workspace.path)?;
    let session_id = input.workspace.branch.clone();

    // norn takes the projected prompt positionally; --print is headless,
    // --resume-if-exists creates or resumes this session, --workspace-root confines file
    // tools, --output-schema constrains the structured result to the
    // dev-report shape, and --output-format json emits the final envelope we
    // decode.
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--reasoning-effort",
            "x-high",
            "--session-id",
            &session_id,
            "--resume-if-exists",
            "--workspace-root",
            &input.workspace.path,
            "--output-schema",
            DEV_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &input.prompt,
        ],
        &input.workspace.path,
        "norn dev",
    )?;
    let dev_report = parse_report::<DevReport>(&command_run, "norn dev")?;
    Ok(StartupResult::Developed { dev_report })
}

/// `scout`: the read-only orientation round in its own deterministic norn
/// session (`<branch>-scout`, CN4).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither the bare report nor the `{"output":
/// …}` envelope.
pub fn scout(shell: &Shell, input: ScoutInput) -> Result<ScoutReport, ActivityFailure> {
    let ScoutInput { workspace, prompt } = input;
    require_workspace_dir(&workspace.path)?;
    let session_id = format!("{}-scout", workspace.branch);
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--reasoning-effort",
            "medium",
            "--session-id",
            &session_id,
            "--resume-if-exists",
            "--workspace-root",
            &workspace.path,
            "--output-schema",
            SCOUT_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &prompt,
        ],
        &workspace.path,
        "norn scout",
    )?;
    parse_report::<ScoutReport>(&command_run, "norn scout")
}

/// `dev_review`: the adversarial reviewer round in its own deterministic norn
/// session (`<branch>-review` — NEVER the dev session, CN4).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither the bare report nor the `{"output":
/// …}` envelope.
pub fn dev_review(shell: &Shell, input: ReviewInput) -> Result<ReviewReport, ActivityFailure> {
    let ReviewInput { workspace, prompt } = input;
    require_workspace_dir(&workspace.path)?;
    let session_id = format!("{}-review", workspace.branch);
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--reasoning-effort",
            "x-high",
            "--session-id",
            &session_id,
            "--resume-if-exists",
            "--workspace-root",
            &workspace.path,
            "--output-schema",
            REVIEW_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &prompt,
        ],
        &workspace.path,
        "norn review",
    )?;
    parse_report::<ReviewReport>(&command_run, "norn review")
}

/// `dev_resume`: resume the same dev agent session with feedback (scoped-check
/// diagnostics). Returns a FULL replacement dev report.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `norn` cannot run, exits non-zero, or
/// answers with output matching neither the bare report nor the `{"output":
/// …}` envelope.
pub fn dev_resume(shell: &Shell, input: ResumeInput) -> Result<DevReport, ActivityFailure> {
    // Resume by the deterministic session id; the feedback is the prompt.
    // Like the local implementation, resume carries no --workspace-root (the
    // workspace root is not on ResumeInput yet — TODO(meridian) in locals).
    let ResumeInput {
        session_id,
        feedback,
    } = input;
    let command_run = require_run(
        shell,
        "norn",
        &[
            "--print",
            "--reasoning-effort",
            "x-high",
            "--resume",
            &session_id,
            "--output-schema",
            DEV_OUTPUT_SCHEMA,
            "--output-format",
            "json",
            &feedback,
        ],
        ".",
        "norn resume",
    )?;
    parse_report::<DevReport>(&command_run, "norn resume")
}

/// `scoped_checks`: compute the affected package set from the dependency
/// graph, then run diagnostics limited to it. An empty affected set falls
/// back LOUDLY to a named workspace-wide scope; zero checks are never run
/// silently.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `yg` cannot run or when the
/// affected-set query exits non-zero. A failing diagnostics run is NOT an
/// error — it is the recorded fail verdict carrying the diagnostics.
pub fn scoped_checks(shell: &Shell, input: ScopedInput) -> Result<CheckResult, ActivityFailure> {
    let ScopedInput {
        workspace,
        files_touched,
    } = input;
    require_workspace_dir(&workspace.path)?;
    // Affected packages come from the dependency graph: one bare crate name
    // per line (direct-only = the crates that actually contain the changed
    // files; the gate runs broad).
    let mut affected_args = vec!["graph", "affected", "--plain", "--direct-only"];
    affected_args.extend(files_touched.iter().map(String::as_str));
    let affected_run = require_run(
        shell,
        "yg",
        &affected_args,
        &workspace.path,
        "yg graph affected",
    )?;
    let packages: Vec<String> = affected_run
        .output
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();

    if packages.is_empty() {
        // No affected packages — fall back LOUDLY to a named workspace-wide
        // scope. Wording identical to the local implementation.
        let scope = "workspace-wide fallback: affected scoping returned an empty set";
        check_with(
            shell,
            &["diagnostics", "check", "--workspace", "--format", "json"],
            &workspace,
            Vec::new(),
            scope,
        )
    } else {
        // One scoped diagnostics run over exactly the affected packages.
        let mut args = vec!["diagnostics", "check", "--format", "json"];
        for name in &packages {
            args.push("--package");
            args.push(name);
        }
        let scope = format!("affected: {}", packages.join(", "));
        // `args` borrows the package names, so the owned set is cloned into
        // the result.
        check_with(shell, &args, &workspace, packages.clone(), &scope)
    }
}

/// Run one `yg diagnostics check` invocation and shape the verdict. Exit
/// zero is a pass; a non-zero exit carries the diagnostics output. A command
/// that cannot run at all is a loud terminal activity failure.
fn check_with(
    shell: &Shell,
    args: &[&str],
    workspace: &Workspace,
    affected_modules: Vec<String>,
    scope: &str,
) -> Result<CheckResult, ActivityFailure> {
    match shell.run("yg", args, &workspace.path) {
        Ok(command_run) => {
            let verdict = if command_run.succeeded() {
                CheckVerdict::Pass
            } else {
                CheckVerdict::Fail {
                    diagnostics: command_run.output,
                }
            };
            Ok(CheckResult {
                verdict,
                affected_modules,
                checked_scope: scope.to_owned(),
            })
        }
        Err(failure) => Err(ActivityFailure::terminal(format!(
            "yg diagnostics check: {}",
            failure.message()
        ))),
    }
}

/// `full_checks`: the authoritative gate — the full workspace diagnostics
/// run, stricter than the fast scoped inner loop.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the gate scope has no implementation or
/// when `yg` cannot run. A failing workspace sweep is NOT an error — it is
/// the recorded fail verdict carrying the report.
pub fn full_checks(shell: &Shell, input: GateInput) -> Result<GateResult, ActivityFailure> {
    let GateInput {
        workspace, scope, ..
    } = input;
    require_workspace_dir(&workspace.path)?;
    match scope {
        GateScope::WorkspaceWide => {
            match shell.run(
                "yg",
                &["diagnostics", "check", "--workspace", "--format", "json"],
                &workspace.path,
            ) {
                Ok(command_run) => {
                    let verdict = if command_run.succeeded() {
                        GateVerdict::Pass
                    } else {
                        GateVerdict::Fail {
                            report: command_run.output,
                        }
                    };
                    Ok(GateResult { verdict })
                }
                Err(failure) => Err(ActivityFailure::terminal(format!(
                    "yg diagnostics check --workspace: {}",
                    failure.message()
                ))),
            }
        }
        // The affected-closure gate scope is a typed seam only — nothing
        // guessed until the graph-derived closure is trusted.
        GateScope::AffectedClosure { .. } => Err(ActivityFailure::terminal(
            "affected-closure gate scope has no local implementation \
             (TODO(meridian): complete affected closure from the workspace graph)",
        )),
    }
}

/// `request_review`: emit a review request. It only requests — the verdict
/// arrives later on the `review_verdict` signal.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `meridian` cannot run, exits non-zero,
/// or answers without a parseable response envelope.
pub fn request_review(shell: &Shell, input: ReviewRequest) -> Result<ReviewAck, ActivityFailure> {
    // CONFIRMED against the real CLI (live runs, 2026-06-13):
    // `meridian review request <BRANCH> --reviewer <NAME>... --as Meridian`.
    // The branch positional must come FIRST: `--reviewer` is greedy
    // multi-value and swallows a trailing positional as another reviewer.
    // `--as` names the requesting identity — always the Meridian system
    // member (the CLI refuses to guess when the workspace has several
    // members). The meridian workspace resolves from the CLI's own global
    // config.
    let ReviewRequest {
        workspace,
        reviewers,
        ..
    } = input;
    require_workspace_dir(&workspace.path)?;
    let mut args: Vec<String> = vec![
        "review".to_owned(),
        "request".to_owned(),
        workspace.branch.clone(),
    ];
    for reviewer in &reviewers {
        args.push("--reviewer".to_owned());
        args.push(reviewer.clone());
    }
    args.push("--as".to_owned());
    args.push("Meridian".to_owned());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let command_run = require_run(
        shell,
        "meridian",
        &arg_refs,
        &workspace.path,
        "meridian review request",
    )?;
    // CONFIRMED against the real CLI (live run, 2026-06-13): the response
    // envelope is `{"branch": .., "reviewers": [{"name", "dm_status", ..}],
    // ..}` — there is no request id. The branch IS the request's identity
    // (meridian persists `pending_reviewers` against the branch lifecycle),
    // so the recorded ack carries it. Every requested reviewer must have
    // been notified (`dm_status: "sent"`); anything else fails loudly.
    let response: ReviewRequestResponse = require_json(&command_run, "meridian review request")?;
    if let Some(unsent) = response
        .reviewers
        .iter()
        .find(|reviewer| reviewer.dm_status != "sent")
    {
        return Err(ActivityFailure::terminal(format!(
            "meridian review request did not notify reviewer {}: dm_status was {:?}",
            unsent.name, unsent.dm_status
        )));
    }
    Ok(ReviewAck {
        request_id: response.branch,
    })
}

/// `land`: commit the dev rounds' files, then either push + create a PR
/// (remote) or merge via `yg` (local).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when `git`/`yg`/`gh` cannot run, when the
/// commit exits non-zero, or when the push/merge/PR-create exits non-zero.
pub fn land(shell: &Shell, input: LandInput) -> Result<Landed, ActivityFailure> {
    let LandInput {
        workspace,
        repo_root,
        base_ref,
        dev_result,
        clone_url,
    } = input;
    require_workspace_dir(&workspace.path)?;

    require_run(shell, "git", &["add", "-A"], &workspace.path, "git add")?;
    let message = format!("{}: {}", workspace.branch, dev_result.summary);
    require_run(
        shell,
        "git",
        &["commit", "-m", &message],
        &workspace.path,
        "git commit",
    )?;

    if clone_url.is_some() {
        land_remote(shell, &workspace, &base_ref, &dev_result.summary)
    } else {
        land_local(shell, &workspace, &repo_root, &base_ref)
    }
}

/// Push the branch and create a PR via `gh`.
fn land_remote(
    shell: &Shell,
    workspace: &Workspace,
    base_ref: &str,
    summary: &str,
) -> Result<Landed, ActivityFailure> {
    require_run(
        shell,
        "git",
        &["push", "-u", "origin", &workspace.branch],
        &workspace.path,
        "git push",
    )?;

    let title = format!("{}: {}", workspace.branch, summary);
    require_run(
        shell,
        "gh",
        &[
            "pr",
            "create",
            "--title",
            &title,
            "--body",
            &format!("Automated PR for branch {}", workspace.branch),
            "--base",
            base_ref,
            "--head",
            &workspace.branch,
        ],
        &workspace.path,
        "gh pr create",
    )?;

    Ok(Landed {
        branch: workspace.branch.clone(),
        merged_into: base_ref.to_owned(),
    })
}

/// Merge via `yg` — the local landing path.
fn land_local(
    shell: &Shell,
    workspace: &Workspace,
    repo_root: &str,
    base_ref: &str,
) -> Result<Landed, ActivityFailure> {
    require_run(
        shell,
        "yg",
        &["branch", "merge", &workspace.branch, "--yes"],
        repo_root,
        "yg branch merge",
    )?;
    Ok(Landed {
        branch: workspace.branch.clone(),
        merged_into: base_ref.to_owned(),
    })
}

/// `enrich_brief`: append one stage report or the execution block into the
/// brief file inside the run's worktree (ADR-007, ADR-009), mirroring
/// `locals.enrich_brief`. The write is guarded by CN3: the on-disk brief's
/// authored subset must equal the handed document's before anything is
/// written — divergence is a terminal failure naming the brief path and the
/// first divergent field, never a silent overwrite. A missing, unreadable, or
/// undecodable brief file is a broken worktree: a can't-execute condition that
/// fails terminally (CN5), never a retry or a skip.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when the brief file cannot be read or decoded,
/// when an authored field diverges, when a report names an unknown
/// requirement, or when the merged document cannot be written.
pub fn enrich_brief(_shell: &Shell, input: EnrichInput) -> Result<BriefDocument, ActivityFailure> {
    let EnrichInput {
        workspace,
        document,
        enrichment,
    } = input;
    require_workspace_dir(&workspace.path)?;
    // The design-system layout is a format constraint (briefs/ is what
    // validate.py keys its brief-schema detection on), so the path derives
    // from the handed document — never from a workflow-supplied guess.
    let brief_path: PathBuf = [
        workspace.path.as_str(),
        "docs",
        "design",
        document.cluster.as_str(),
        "briefs",
        &format!("{}.json", document.id),
    ]
    .iter()
    .collect();
    let brief_path_display = brief_path.display().to_string();

    let raw = std::fs::read_to_string(&brief_path).map_err(|source| {
        ActivityFailure::terminal(format!(
            "enrich_brief: cannot read {brief_path_display}: {source}"
        ))
    })?;
    let on_disk: BriefDocument = serde_json::from_str(&raw).map_err(|source| {
        ActivityFailure::terminal(format!(
            "enrich_brief: brief file {brief_path_display} failed to decode: {source}"
        ))
    })?;
    if let Some(field) = crate::enrich::authored_divergence(&on_disk, &document) {
        return Err(ActivityFailure::terminal(format!(
            "enrich_brief: authored field {field} in {brief_path_display} \
             diverges from the handed document; refusing to write (CN3)"
        )));
    }
    let merged = crate::enrich::apply(document, &enrichment)
        .map_err(|error| ActivityFailure::terminal(format!("enrich_brief: {error}")))?;
    let encoded = serde_json::to_string(&merged).map_err(|source| {
        ActivityFailure::terminal(format!(
            "enrich_brief: cannot encode merged document: {source}"
        ))
    })?;
    std::fs::write(&brief_path, encoded).map_err(|source| {
        ActivityFailure::terminal(format!(
            "enrich_brief: cannot write {brief_path_display}: {source}"
        ))
    })?;
    Ok(merged)
}

/// `assemble_wave`: resolve, order, and refuse a dispatch wave (BD-006),
/// mirroring `locals.assemble_wave`. The ledger-reading, reference-resolving
/// logic lives in [`crate::assemble`] (the only such code in the worker, CN1);
/// this is the handler seam the worker registers. It takes no `Shell` — like
/// the local, it performs file IO directly and shells nothing.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] on a refusal or any can't-execute condition
/// (unreadable ledger, undecodable brief, dependency-blocked, coverage-broken,
/// or cyclic wave).
pub fn assemble_wave(
    _shell: &Shell,
    input: AssembleInput,
) -> Result<AssembledWave, ActivityFailure> {
    crate::assemble::assemble_wave(input)
}

/// `teardown_workspace`: remove the workspace directory and clean up norn
/// sessions. The workflow invokes this ONLY on the success path, after the
/// work has landed on the tree parent — never on a failure path, where the
/// worktree, branch, and committed dev rounds must survive intact so the run
/// can be reopened and resumed from the failed step.
///
/// Remote clone workspaces live at `<workspace root>/<run id>/repo`;
/// teardown deletes the per-run parent (and with it the repo) ONLY when
/// that parent sits strictly under the resolved workspace root, and then
/// sweeps this run's own `<run id>.superseded-<unique>` siblings (the
/// renamed-aside partial provision attempts — on the success path the
/// landed work is pushed, so they hold nothing salvageable). The
/// containment check runs on CANONICALIZED paths — `Path::starts_with` is
/// lexical and would pass `<root>/x/../victim`, while `remove_dir_all`
/// resolves the `..` at the filesystem level. A path outside the root (or
/// one that no longer exists, or a root that cannot be canonicalized) is
/// not ours to delete and is left untouched with `cleaned: false` — no
/// temp-dir parent heuristics: guessing at parents is how #175 deleted
/// survivors.
///
/// # Errors
///
/// Terminal [`ActivityFailure`] if a directory this teardown owns cannot be
/// removed and still exists, or (for a remote workspace) when the workspace
/// root cannot be resolved. Missing paths are already clean — not an error.
pub fn teardown_workspace(
    _shell: &Shell,
    input: TeardownInput,
) -> Result<TornDown, ActivityFailure> {
    let workspace_path_owned = input.workspace.path;
    let branch = input.workspace.branch;
    let workspace_path = std::path::Path::new(&workspace_path_owned);

    let mut cleaned = false;
    match input.workspace.placement {
        Placement::Remote => {
            let root = resolve_workspace_root()?;
            // Canonicalize BOTH sides before the containment check: the
            // recorded path arrives over the wire from durable history, and
            // a lexical `starts_with` would pass a `..`-escaping parent. A
            // parent (or root) that cannot be canonicalized does not exist
            // — already clean, refuse to touch anything.
            let owned_parent = workspace_path
                .parent()
                .and_then(|parent| std::fs::canonicalize(parent).ok())
                .filter(|parent| {
                    std::fs::canonicalize(&root)
                        .is_ok_and(|root| parent.starts_with(&root) && *parent != root)
                });
            if let Some(parent) = owned_parent {
                // Sweep the superseded siblings BEFORE the run directory
                // itself: if the sweep fails terminally, a retried teardown
                // still finds the parent and owns the whole cleanup; the
                // other order would strand the siblings behind a refusal.
                remove_superseded_siblings(&parent)?;
                std::fs::remove_dir_all(&parent).map_err(|source| {
                    ActivityFailure::terminal(format!(
                        "teardown: cannot remove workspace run directory {}: {source}",
                        parent.display()
                    ))
                })?;
                cleaned = true;
            }
            // A remote workspace whose parent is NOT under the resolved
            // root is refused, never deleted: `cleaned` stays false and the
            // directory survives.
        }
        Placement::Local => {
            // A local worktree is owned by the run; remove it as before.
            if workspace_path.exists() {
                std::fs::remove_dir_all(workspace_path).map_err(|source| {
                    ActivityFailure::terminal(format!(
                        "teardown: cannot remove workspace {workspace_path_owned}: {source}",
                    ))
                })?;
                cleaned = true;
            }
        }
    }

    // Clean up norn sessions for this branch.
    if let Some(home) = std::env::var_os("HOME") {
        let sessions_dir = std::path::PathBuf::from(home).join(".norn/sessions");
        if sessions_dir.is_dir() {
            for suffix in &["", "-scout", "-review"] {
                let session_file = sessions_dir.join(format!("{branch}{suffix}.jsonl"));
                let _ = std::fs::remove_file(session_file);
            }
        }
    }

    Ok(TornDown { branch, cleaned })
}

/// Remove the `<run key>.superseded-<unique>` siblings that this run's own
/// earlier partial provision attempts left behind (a crash-recovered
/// provision renames its predecessor aside rather than deleting it — see
/// [`claim_run_directory`]). The sweep touches ONLY direct children of the
/// run directory's parent whose names carry the run key plus the exact
/// `.superseded-` marker: no other run's directory can match, because run
/// keys are single path components and the marker is minted solely by this
/// worker's rename-aside.
fn remove_superseded_siblings(run_dir: &std::path::Path) -> Result<(), ActivityFailure> {
    let (Some(root), Some(run_key)) = (run_dir.parent(), run_dir.file_name()) else {
        return Ok(());
    };
    let prefix = format!("{}.superseded-", run_key.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(root) else {
        // The root vanished mid-teardown: nothing left to sweep.
        return Ok(());
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let stale = root.join(entry.file_name());
            std::fs::remove_dir_all(&stale).map_err(|source| {
                ActivityFailure::terminal(format!(
                    "teardown: cannot remove superseded provision attempt {}: {source}",
                    stale.display()
                ))
            })?;
        }
    }
    Ok(())
}

// --- helpers ----------------------------------------------------------------

/// The `meridian review request` response envelope — only the fields the
/// handler consumes (extra fields tolerated, like the Gleam field decoder).
#[derive(Deserialize)]
struct ReviewRequestResponse {
    branch: String,
    reviewers: Vec<ReviewerNotification>,
}

/// One reviewer's notification outcome inside [`ReviewRequestResponse`].
#[derive(Deserialize)]
struct ReviewerNotification {
    name: String,
    dm_status: String,
}

/// Require a command to run AND exit zero; anything else is a terminal
/// activity failure carrying the command's diagnostics. Wording identical to
/// `locals.require_run`.
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

/// Decode a command's stdout as JSON; malformed output is a terminal
/// activity failure carrying the raw text, like `locals.require_json`.
fn require_json<T: serde::de::DeserializeOwned>(
    command_run: &CliRun,
    context: &str,
) -> Result<T, ActivityFailure> {
    let trimmed = command_run.stdout.trim();
    serde_json::from_str(trimmed).map_err(|_| {
        ActivityFailure::terminal(format!("{context} produced unparseable output: {trimmed}"))
    })
}

/// Decode a norn command's stdout as a stage report, generic over the report
/// type.
///
/// CONFIRMED against real norn (live run, 2026-06-13): `--output-format json`
/// emits a completion envelope with the schema-constrained result under
/// `"output"`, alongside `usage`/`model`/`session_id`/`events` (ignored
/// here). Exactly two shapes are attempted — the bare report (the fake-CLI
/// shims emit it raw), then norn's `{"output": <report>}` envelope — and if
/// BOTH fail the activity fails terminally carrying the head of the output. No
/// silent fallback beyond that documented two-shape attempt (C31).
fn parse_report<T: DeserializeOwned>(
    command_run: &CliRun,
    context: &str,
) -> Result<T, ActivityFailure> {
    #[derive(Deserialize)]
    struct NornEnvelope<T> {
        output: T,
    }

    // Use stdout only — stderr carries norn's WARN/INFO log lines which
    // corrupt the JSON parse.
    let trimmed = command_run.stdout.trim();
    if let Ok(report) = serde_json::from_str::<T>(trimmed) {
        return Ok(report);
    }
    if let Ok(envelope) = serde_json::from_str::<NornEnvelope<T>>(trimmed) {
        return Ok(envelope.output);
    }

    // Try extracting the first JSON line from stdout as a last resort.
    if let Some(json_line) = first_json_line(trimmed) {
        if let Ok(report) = serde_json::from_str::<T>(json_line) {
            return Ok(report);
        }
        if let Ok(envelope) = serde_json::from_str::<NornEnvelope<T>>(json_line) {
            return Ok(envelope.output);
        }
    }

    Err(ActivityFailure::terminal(format!(
        "{context} produced unparseable output \
         (tried the bare report shape and norn's {{\"output\": …}} envelope): {}",
        head(trimmed, UNPARSEABLE_OUTPUT_HEAD)
    )))
}

/// Extract the first line that looks like a complete JSON object or array.
fn first_json_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| {
        (line.starts_with('{') && line.ends_with('}'))
            || (line.starts_with('[') && line.ends_with(']'))
    })
}

/// First `limit` characters of `text`, truncated on a char boundary.
fn head(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((boundary, _)) => &text[..boundary],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::workspace_root_from;

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
        // A relative root resolves against the worker CWD at each call
        // site, so the recorded history's path silently changes meaning
        // after a restart from a different directory — refuse it loudly.
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
}
