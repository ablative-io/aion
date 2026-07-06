//! Mechanical post-turn git for the agent roles that produce work in the
//! brief workspace: commit the test-author's authored tests and the
//! developer's fix work on the brief branch.
//!
//! DOCTRINE: agents do not run git — the worker machinery does the mechanical
//! git around them. DESIGN.md flow step 4 requires the authored tests
//! "committed on the fix branch as its first commit(s)" (gate 1 verifies a
//! clean worktree and diffs the committed set), and the fix report's
//! `commits` field / the ledger's `fix_commit` ride on the developer's work
//! actually being committed. Without these steps every brief dies at gate 1
//! with "authored tests are not committed" (the live drill's exact failure),
//! and the fix branch retains nothing the developer did.
//!
//! Both commits are deliberately narrow and honest:
//!
//! - the TEST-AUTHOR commit stages `git add -- <explicit paths>` ONLY — each
//!   runnable manifest entry's `test_file`, never `git add -A`/`.`, so stray
//!   workspace debris can never ride into the contract commit. A claimed
//!   `test_file` missing on disk is an ERROR naming the path.
//! - the DEVELOPER commit stages `git add --update` (tracked modifications
//!   and deletions — the fix-report schema declares NO changed-files field,
//!   so the tracked delta is the only honest set; never an untracked sweep)
//!   plus the report's `new_tests` entries that are path claims (contain a
//!   `/`; entries like `module::test_name` are test NAMES, ignored for
//!   staging). A path-claim that does not exist on disk is an ERROR naming
//!   it. The staged set is recorded in the commit body.
//! - each role commits under its own scoped identity via `-c`, so the commit
//!   is attributable to the machinery without touching the workspace's git
//!   config.
//! - nothing staged (all `could_not_reproduce`, a bounce-everything fix
//!   round, or an activity retry after a successful commit) skips green and
//!   idempotently — never an empty-commit failure.
//!
//! REALITY WINS on the fix report's `commits`: the schema requires the field
//! but the agent never ran git, so whatever it asserts is fabricated. After
//! the mechanical commit the harness rewrites the activity RESULT's `commits`
//! to the branch head that actually embodies the reported work
//! ([`rewrite_report_commits`]), so the ledger/verdict downstream see a real
//! hash.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::shell::{CliRun, Shell};

/// The test-author commit's scoped committer name — machinery identity, not
/// an agent, not the operator.
pub const TEST_AUTHOR_COMMIT_NAME: &str = "remediation-test-author";

/// The test-author commit's scoped committer email.
pub const TEST_AUTHOR_COMMIT_EMAIL: &str = "test-author@remediation.local";

/// The developer commit's scoped committer name.
pub const DEVELOPER_COMMIT_NAME: &str = "remediation-developer";

/// The developer commit's scoped committer email.
pub const DEVELOPER_COMMIT_EMAIL: &str = "developer@remediation.local";

/// The slice of an agent activity INPUT the commit step needs: the brief
/// identity (for the commit message) and the provisioned workspace the work
/// lives in. Deserialized tolerantly from the workflow's context JSON
/// (`codecs.test_author_input_codec` / `codecs.developer_input_codec` are the
/// authoritative wire shapes; extra fields are ignored).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CommitContext {
    /// The brief this turn serves.
    pub brief: BriefIdentity,
    /// The brief's provisioned worktree (absolute path).
    pub workspace_path: String,
}

/// The one brief field the commit step reads.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct BriefIdentity {
    /// The brief id (rides in the commit message).
    pub id: String,
}

/// The slice of the test-author's manifest OUTPUT the commit step reads:
/// per entry, the claimed test file and the `could_not_reproduce` flag.
/// Extra fields (names, signatures, evidence) are the workflow's concern.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AuthoredManifest {
    /// The manifest entries, one per finding.
    pub entries: Vec<AuthoredManifestEntry>,
}

/// One manifest entry's commit-relevant fields.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AuthoredManifestEntry {
    /// The workspace-relative test file this entry claims.
    pub test_file: String,
    /// True when the finding could not be reproduced — nothing was authored
    /// for it.
    pub could_not_reproduce: bool,
}

/// The slice of the developer's fix-report OUTPUT the commit step reads.
/// `fix-report.schema.json` declares no changed-files field; `new_tests` is
/// the only channel that can name new (untracked) files, and its entries may
/// be test NAMES (`module::case`) rather than paths — see
/// [`commit_fix_work`]'s staging rules.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct FixReportSlice {
    /// Tests the developer added — path claims (containing `/`) are staged
    /// explicitly; name-shaped entries are ignored for staging.
    pub new_tests: Vec<String>,
}

/// What the test-author commit step did — recorded, never silent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The authored tests were committed.
    Committed {
        /// The created commit's hash.
        commit: String,
        /// The explicit paths the commit carries.
        paths: Vec<String>,
    },
    /// Nothing needed committing (all `could_not_reproduce`, or the paths
    /// were already committed by a previous attempt).
    Skipped {
        /// Why nothing was committed.
        reason: String,
    },
}

/// What the developer commit step did. BOTH variants carry the branch head
/// afterwards: that hash is what the activity result's `commits` is rewritten
/// to (reality wins — on a skip, the head already embodies the reported work:
/// a previous attempt's commit, or the unchanged tests commit when the round
/// changed nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FixCommitOutcome {
    /// The fix work was committed.
    Committed {
        /// The created commit's hash (the branch head afterwards).
        commit: String,
        /// The staged paths the commit carries (from git, not from claims).
        paths: Vec<String>,
    },
    /// Nothing was staged; the branch head is unchanged.
    Skipped {
        /// The branch head at skip time.
        head: String,
        /// Why nothing was committed.
        reason: String,
    },
}

impl FixCommitOutcome {
    /// The branch head that embodies the reported work after the step.
    #[must_use]
    pub fn head(&self) -> &str {
        match self {
            Self::Committed { commit, .. } => commit,
            Self::Skipped { head, .. } => head,
        }
    }
}

/// Parse an agent activity input into the commit-relevant context.
///
/// # Errors
///
/// Returns a message naming what was missing/malformed — the input codecs are
/// supposed to always carry `brief.id` and `workspace_path`, so a failure
/// here is a wiring fault that must surface loudly.
pub fn context_from_input(context_json: &str) -> Result<CommitContext, String> {
    serde_json::from_str(context_json).map_err(|error| {
        format!(
            "agent activity input does not carry the commit context \
             (brief.id + workspace_path): {error}"
        )
    })
}

/// Parse the test-author's terminal output into the commit-relevant manifest
/// slice. Tolerates the double-encoded form (a JSON string containing the
/// manifest JSON) alongside the plain object — schema-constrained Norn runs
/// emit the object itself.
///
/// # Errors
///
/// Returns a message when the output is not a manifest-shaped JSON document.
pub fn manifest_from_output(bytes: &[u8]) -> Result<AuthoredManifest, String> {
    let (value, _) = output_json(bytes, "test_author")?;
    serde_json::from_value(value)
        .map_err(|error| format!("test_author output is not a test manifest: {error}"))
}

/// Parse the developer's terminal output into the commit-relevant fix-report
/// slice (same envelope tolerance as [`manifest_from_output`]).
///
/// # Errors
///
/// Returns a message when the output is not a fix-report-shaped JSON
/// document.
pub fn fix_report_from_output(bytes: &[u8]) -> Result<FixReportSlice, String> {
    let (value, _) = output_json(bytes, "developer")?;
    serde_json::from_value(value)
        .map_err(|error| format!("developer output is not a fix report: {error}"))
}

/// Rewrite the fix report's `commits` field to the ONE real branch head the
/// mechanical commit step produced/verified, preserving the output's envelope
/// form (plain object stays an object; a double-encoded string stays a
/// string). Reality wins: the agent never ran git, so its asserted hashes are
/// fabricated by construction and downstream (ledger `fix_commit`, verdict
/// evidence) must see the real one.
///
/// # Errors
///
/// Returns a message when the output is not a JSON object document.
pub fn rewrite_report_commits(bytes: &[u8], head: &str) -> Result<Vec<u8>, String> {
    let (mut value, double_encoded) = output_json(bytes, "developer")?;
    let serde_json::Value::Object(ref mut object) = value else {
        return Err("developer output is not a JSON object; cannot rewrite `commits`".to_owned());
    };
    object.insert(
        "commits".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::String(head.to_owned())]),
    );
    let rewritten = if double_encoded {
        let inner = serde_json::to_string(&value)
            .map_err(|error| format!("rewritten fix report is not encodable: {error}"))?;
        serde_json::to_vec(&serde_json::Value::String(inner))
    } else {
        serde_json::to_vec(&value)
    };
    rewritten.map_err(|error| format!("rewritten fix report is not encodable: {error}"))
}

/// Decode an agent output payload to its JSON value, unwrapping the
/// double-encoded string form when present. Returns the value and whether it
/// was double-encoded (so a rewrite can preserve the envelope).
fn output_json(bytes: &[u8], role: &str) -> Result<(serde_json::Value, bool), String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("{role} output is not JSON: {error}"))?;
    match value {
        serde_json::Value::String(inner) => {
            let unwrapped = serde_json::from_str(&inner).map_err(|error| {
                format!("{role} output string does not hold a JSON document: {error}")
            })?;
            Ok((unwrapped, true))
        }
        other => Ok((other, false)),
    }
}

/// Commit the authored tests in `workspace_path` on the brief branch:
/// `git add -- <each runnable entry's test_file>` then one commit under the
/// scoped machinery identity, message `test(<brief_id>): authored failing
/// tests`.
///
/// # Errors
///
/// - a claimed `test_file` missing on disk (the agent claimed a test it did
///   not write) — the error names the path;
/// - `git` unable to run, or `add`/`commit`/`rev-parse` exiting non-zero —
///   the error carries the command context and captured output.
pub fn commit_authored_tests(
    shell: &Shell,
    workspace_path: &str,
    brief_id: &str,
    manifest: &AuthoredManifest,
) -> Result<CommitOutcome, String> {
    let paths = authored_paths(manifest);
    if paths.is_empty() {
        return Ok(CommitOutcome::Skipped {
            reason: "no runnable manifest entry names a test file (all \
                     could_not_reproduce); nothing to commit"
                .to_owned(),
        });
    }

    for path in &paths {
        require_file_exists(workspace_path, path, "test author")?;
    }

    let mut add_args = vec!["add", "--"];
    add_args.extend(paths.iter().map(String::as_str));
    require_git_ok(shell, workspace_path, &add_args, "git add (authored tests)")?;

    // Idempotency for activity retries: when a previous attempt already
    // committed these paths, the add stages nothing — skip honestly instead
    // of failing on an empty commit.
    if staged_paths(shell, workspace_path)?.is_empty() {
        return Ok(CommitOutcome::Skipped {
            reason: format!(
                "authored test path(s) already committed, nothing staged: {}",
                paths.join(", ")
            ),
        });
    }

    let message = format!("test({brief_id}): authored failing tests");
    commit_as(
        shell,
        workspace_path,
        TEST_AUTHOR_COMMIT_NAME,
        TEST_AUTHOR_COMMIT_EMAIL,
        &message,
        "git commit (authored tests)",
    )?;

    Ok(CommitOutcome::Committed {
        commit: rev_parse_head(shell, workspace_path)?,
        paths,
    })
}

/// Commit the developer's fix work in `workspace_path` on the brief branch.
///
/// STAGING RULES (the fix-report schema declares no changed-files field):
///
/// 1. every `new_tests` entry that is a PATH CLAIM (contains a `/`) must
///    exist on disk — a missing one is an error naming it (the developer
///    claimed a test it did not write); name-shaped entries
///    (`module::case`, no `/`) are test names, not path claims, and are
///    ignored for staging;
/// 2. `git add --update` stages every tracked modification/deletion — never
///    an untracked sweep;
/// 3. the existing path claims are staged explicitly (`git add -- <paths>`)
///    — the only channel by which NEW files enter the commit.
///
/// The staged set (as reported by git, not as claimed) is recorded in the
/// commit body. One commit, scoped identity `remediation-developer`, message
/// `fix(<brief_id>): apply remediation` (the schema has no summary field).
/// Nothing staged skips green with the unchanged head — idempotent across
/// activity retries and honest for a bounce-everything round.
///
/// # Errors
///
/// - a `new_tests` path claim missing on disk — the error names the path;
/// - `git` unable to run, or any staging/commit command exiting non-zero.
pub fn commit_fix_work(
    shell: &Shell,
    workspace_path: &str,
    brief_id: &str,
    report: &FixReportSlice,
) -> Result<FixCommitOutcome, String> {
    let claims = new_file_claims(report);
    for path in &claims {
        require_file_exists(workspace_path, path, "developer")?;
    }

    require_git_ok(
        shell,
        workspace_path,
        &["add", "--update"],
        "git add --update (developer's tracked changes)",
    )?;
    if !claims.is_empty() {
        let mut add_args = vec!["add", "--"];
        add_args.extend(claims.iter().map(String::as_str));
        require_git_ok(
            shell,
            workspace_path,
            &add_args,
            "git add (developer's new test files)",
        )?;
    }

    let staged = staged_paths(shell, workspace_path)?;
    if staged.is_empty() {
        return Ok(FixCommitOutcome::Skipped {
            head: rev_parse_head(shell, workspace_path)?,
            reason: "no tracked changes and no new report-named test files; \
                     nothing to commit (already committed, or the round \
                     changed nothing)"
                .to_owned(),
        });
    }

    let message = format!(
        "fix({brief_id}): apply remediation\n\nStaged paths:\n{}",
        staged
            .iter()
            .map(|path| format!("  {path}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    commit_as(
        shell,
        workspace_path,
        DEVELOPER_COMMIT_NAME,
        DEVELOPER_COMMIT_EMAIL,
        &message,
        "git commit (developer fix work)",
    )?;

    Ok(FixCommitOutcome::Committed {
        commit: rev_parse_head(shell, workspace_path)?,
        paths: staged,
    })
}

/// The explicit path set the test-author commit carries: each entry with
/// work behind it (`could_not_reproduce == false`) contributes its non-empty
/// `test_file`, deduplicated keeping manifest order.
fn authored_paths(manifest: &AuthoredManifest) -> Vec<String> {
    dedup_keeping_order(
        manifest
            .entries
            .iter()
            .filter(|entry| !entry.could_not_reproduce)
            .map(|entry| entry.test_file.trim())
            .filter(|path| !path.is_empty()),
    )
}

/// The fix report's `new_tests` entries that carry PATH claims. Entries come
/// in three shapes seen live: a bare path (`tests/clamp_edges.rs`), a
/// path-qualified test (`tests/clamp_edges.rs::case_name` — the path is the
/// part before the first `::`), and a bare test name (`module::case`, no `/`
/// in its path part) which names a test, not a file, and stages nothing.
fn new_file_claims(report: &FixReportSlice) -> Vec<String> {
    dedup_keeping_order(
        report
            .new_tests
            .iter()
            .map(|entry| entry.trim())
            .map(|entry| entry.split("::").next().unwrap_or(entry).trim())
            .filter(|path_part| path_part.contains('/')),
    )
}

fn dedup_keeping_order<'candidates>(
    candidates: impl Iterator<Item = &'candidates str>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    candidates
        .filter(|path| seen.insert((*path).to_owned()))
        .map(str::to_owned)
        .collect()
}

/// A claimed workspace-relative file must exist on disk — the agent claimed
/// work it did not do otherwise, and that must surface, not be swallowed.
fn require_file_exists(workspace_path: &str, path: &str, claimant: &str) -> Result<(), String> {
    if Path::new(workspace_path).join(path).is_file() {
        Ok(())
    } else {
        Err(format!(
            "report claims test file `{path}` but it does not exist in the \
             workspace `{workspace_path}` — the {claimant} claimed a test it \
             did not write"
        ))
    }
}

/// The currently staged paths (index vs HEAD).
fn staged_paths(shell: &Shell, workspace_path: &str) -> Result<Vec<String>, String> {
    let staged = require_git_ok(
        shell,
        workspace_path,
        &["diff", "--cached", "--name-only"],
        "git diff --cached (staged set)",
    )?;
    Ok(staged
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// One commit under a scoped machinery identity (repo config untouched).
fn commit_as(
    shell: &Shell,
    workspace_path: &str,
    name: &str,
    email: &str,
    message: &str,
    context: &str,
) -> Result<(), String> {
    let user_name = format!("user.name={name}");
    let user_email = format!("user.email={email}");
    require_git_ok(
        shell,
        workspace_path,
        &["-c", &user_name, "-c", &user_email, "commit", "-m", message],
        context,
    )?;
    Ok(())
}

fn rev_parse_head(shell: &Shell, workspace_path: &str) -> Result<String, String> {
    let head = require_git_ok(
        shell,
        workspace_path,
        &["rev-parse", "HEAD"],
        "git rev-parse HEAD",
    )?;
    Ok(head.stdout.trim().to_owned())
}

/// Run one git command in the workspace; unrunnable OR non-zero is an error
/// carrying the context and captured output (the commit steps have no
/// recorded-data exits — every red is a fault).
fn require_git_ok(
    shell: &Shell,
    workspace_path: &str,
    args: &[&str],
    context: &str,
) -> Result<CliRun, String> {
    let run = shell
        .run("git", args, workspace_path)
        .map_err(|failure| format!("{context}: {}", failure.message()))?;
    if run.succeeded() {
        Ok(run)
    } else {
        Err(format!(
            "{context} exited {}: {}",
            run.exit_status,
            run.output.trim()
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        AuthoredManifest, FixReportSlice, context_from_input, fix_report_from_output,
        manifest_from_output, new_file_claims, rewrite_report_commits,
    };

    #[test]
    fn new_file_claims_takes_the_path_part_before_a_test_qualifier() {
        // The live drill shape that broke run b34f078e: a path-qualified test
        // name must claim only the file, and bare test names claim nothing.
        let report = FixReportSlice {
            new_tests: vec![
                "tests/clamp_edges.rs::clamp_preserves_lower_bound_when_value_is_below_lo"
                    .to_owned(),
                "tests/clamp_edges.rs".to_owned(),
                "module::bare_test_name".to_owned(),
            ],
        };
        assert_eq!(new_file_claims(&report), vec!["tests/clamp_edges.rs"]);
    }

    #[test]
    fn context_parses_the_agent_input_wire_shape() {
        // The exact codec wire shape (extra fields present, as on the wire).
        let context = context_from_input(
            "{\"brief\":{\"id\":\"B-1\",\"finding_ids\":[\"YG-268\"]},\
             \"entries\":[],\"workspace_path\":\"/tmp/ws/child-1\"}",
        )
        .expect("parses");
        assert_eq!(context.brief.id, "B-1");
        assert_eq!(context.workspace_path, "/tmp/ws/child-1");
    }

    #[test]
    fn context_without_a_workspace_path_is_a_loud_error() {
        let error = context_from_input("{\"brief\":{\"id\":\"B-1\"},\"entries\":[]}")
            .expect_err("must fail");
        assert!(error.contains("workspace_path"), "error: {error}");
    }

    #[test]
    fn manifest_parses_the_schema_object_and_the_double_encoded_string() {
        let object = "{\"brief_id\":\"B-1\",\"entries\":[{\"finding_id\":\"YG-268\",\
                      \"test_names\":[\"t\"],\"test_file\":\"tests/t.rs\",\
                      \"expected_failure_signature\":\"sig\",\"fail_evidence\":\"e\",\
                      \"could_not_reproduce\":false,\"could_not_reproduce_reason\":null,\
                      \"manual_acceptance\":null}]}";
        let expected = AuthoredManifest {
            entries: vec![super::AuthoredManifestEntry {
                test_file: "tests/t.rs".to_owned(),
                could_not_reproduce: false,
            }],
        };
        assert_eq!(manifest_from_output(object.as_bytes()).unwrap(), expected);

        let double_encoded =
            serde_json::to_vec(&serde_json::Value::String(object.to_owned())).expect("encodable");
        assert_eq!(manifest_from_output(&double_encoded).unwrap(), expected);
    }

    #[test]
    fn non_manifest_output_is_a_loud_error() {
        let error = manifest_from_output(b"{\"unrelated\":true}").expect_err("must fail");
        assert!(error.contains("not a test manifest"), "error: {error}");
    }

    #[test]
    fn fix_report_slice_parses_the_schema_shape() {
        let report = fix_report_from_output(
            "{\"brief_id\":\"B-1\",\"commits\":[\"made-up\"],\
             \"findings_addressed\":[],\"findings_bounced\":[],\"deviations\":[],\
             \"new_tests\":[\"tests/extra.rs\",\"teardown::extra_case\"],\
             \"class_instances_found\":[]}"
                .as_bytes(),
        )
        .expect("parses");
        assert_eq!(
            report.new_tests,
            vec![
                "tests/extra.rs".to_owned(),
                "teardown::extra_case".to_owned()
            ]
        );
    }

    #[test]
    fn rewrite_replaces_fabricated_commits_with_the_real_head() {
        let output = b"{\"brief_id\":\"B-1\",\"commits\":[\"made-up\",\"also-fake\"],\
                       \"new_tests\":[]}";
        let rewritten = rewrite_report_commits(output, "abc123real").expect("rewrites");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(
            value["commits"],
            serde_json::json!(["abc123real"]),
            "reality wins: the asserted hashes are replaced"
        );
        // Everything else survives untouched.
        assert_eq!(value["brief_id"], serde_json::json!("B-1"));
    }

    #[test]
    fn rewrite_preserves_the_double_encoded_envelope() {
        let inner = "{\"brief_id\":\"B-1\",\"commits\":[\"made-up\"]}";
        let double_encoded =
            serde_json::to_vec(&serde_json::Value::String(inner.to_owned())).unwrap();
        let rewritten = rewrite_report_commits(&double_encoded, "abc123real").expect("rewrites");
        // Still a JSON string on the outside...
        let outer: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        let serde_json::Value::String(inner_json) = outer else {
            panic!("the double-encoded envelope must be preserved");
        };
        // ...holding the rewritten report.
        let value: serde_json::Value = serde_json::from_str(&inner_json).unwrap();
        assert_eq!(value["commits"], serde_json::json!(["abc123real"]));
    }
}
