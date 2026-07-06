//! Mechanical post-turn git for the test-author role: commit the authored
//! tests on the brief branch.
//!
//! DOCTRINE: agents do not run git — the worker machinery does the mechanical
//! git around them. DESIGN.md flow step 4 requires the authored tests
//! "committed on the fix branch as its first commit(s)"; gate 1 then verifies
//! a clean worktree and diffs the committed set. Without this step every
//! brief dies at gate 1 with "authored tests are not committed" (the live
//! drill's exact failure), because nobody else ever commits.
//!
//! The commit is deliberately narrow:
//!
//! - `git add -- <explicit paths>` ONLY — each runnable manifest entry's
//!   `test_file`, never `git add -A`/`.`, so stray workspace debris can never
//!   ride into the contract commit.
//! - a scoped identity (`remediation-test-author` /
//!   `test-author@remediation.local`) via `-c`, so the commit is attributable
//!   to the machinery without touching the workspace's git config.
//! - a manifest `test_file` that does not exist on disk is an ERROR naming
//!   the path — the agent claimed a test it did not write, and that must
//!   surface, not be swallowed.
//! - a manifest whose every entry is `could_not_reproduce` (or names no test
//!   file) commits nothing and stays green — there is nothing to commit.
//! - re-running after a successful commit stages nothing and skips — honest
//!   idempotency for activity retries, never an empty-commit failure.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::shell::Shell;

/// The scoped committer name — machinery identity, not an agent, not the
/// operator.
pub const COMMIT_AUTHOR_NAME: &str = "remediation-test-author";

/// The scoped committer email.
pub const COMMIT_AUTHOR_EMAIL: &str = "test-author@remediation.local";

/// The slice of the `test_author` activity INPUT the commit step needs: the
/// brief identity (for the commit message) and the provisioned workspace the
/// authored tests live in. Deserialized tolerantly from the workflow's
/// context JSON (`codecs.test_author_input_codec` is the authoritative wire
/// shape; extra fields are ignored).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct TestAuthorContext {
    /// The brief this authoring turn serves.
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

/// What the commit step did — recorded, never silent.
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

/// Parse the `test_author` activity input into the commit-relevant context.
///
/// # Errors
///
/// Returns a message naming what was missing/malformed — the input codec is
/// supposed to always carry `brief.id` and `workspace_path`, so a failure
/// here is a wiring fault that must surface loudly.
pub fn context_from_input(context_json: &str) -> Result<TestAuthorContext, String> {
    serde_json::from_str(context_json).map_err(|error| {
        format!(
            "test_author activity input does not carry the commit context \
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
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("test_author output is not JSON: {error}"))?;
    let object = match value {
        serde_json::Value::String(inner) => serde_json::from_str(&inner).map_err(|error| {
            format!("test_author output string does not hold manifest JSON: {error}")
        })?,
        other => other,
    };
    serde_json::from_value(object)
        .map_err(|error| format!("test_author output is not a test manifest: {error}"))
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
        if !Path::new(workspace_path).join(path).is_file() {
            return Err(format!(
                "manifest claims test file `{path}` but it does not exist in \
                 the workspace `{workspace_path}` — the test author claimed a \
                 test it did not write"
            ));
        }
    }

    let mut add_args = vec!["add", "--"];
    add_args.extend(paths.iter().map(String::as_str));
    let add = run_git(shell, workspace_path, &add_args, "git add (authored tests)")?;
    if !add.succeeded() {
        return Err(format!(
            "git add of the authored tests exited {}: {}",
            add.exit_status,
            add.output.trim()
        ));
    }

    // Idempotency for activity retries: when a previous attempt already
    // committed these paths, the add stages nothing — skip honestly instead
    // of failing on an empty commit.
    let staged = run_git(
        shell,
        workspace_path,
        &["diff", "--cached", "--name-only"],
        "git diff --cached (staged authored tests)",
    )?;
    if !staged.succeeded() {
        return Err(format!(
            "git diff --cached exited {}: {}",
            staged.exit_status,
            staged.output.trim()
        ));
    }
    if staged.stdout.trim().is_empty() {
        return Ok(CommitOutcome::Skipped {
            reason: format!(
                "authored test path(s) already committed, nothing staged: {}",
                paths.join(", ")
            ),
        });
    }

    let message = format!("test({brief_id}): authored failing tests");
    let user_name = format!("user.name={COMMIT_AUTHOR_NAME}");
    let user_email = format!("user.email={COMMIT_AUTHOR_EMAIL}");
    let commit = run_git(
        shell,
        workspace_path,
        &[
            "-c",
            &user_name,
            "-c",
            &user_email,
            "commit",
            "-m",
            &message,
        ],
        "git commit (authored tests)",
    )?;
    if !commit.succeeded() {
        return Err(format!(
            "git commit of the authored tests exited {}: {}",
            commit.exit_status,
            commit.output.trim()
        ));
    }

    let head = run_git(
        shell,
        workspace_path,
        &["rev-parse", "HEAD"],
        "git rev-parse HEAD (authored-tests commit)",
    )?;
    if !head.succeeded() {
        return Err(format!(
            "git rev-parse HEAD exited {}: {}",
            head.exit_status,
            head.output.trim()
        ));
    }

    Ok(CommitOutcome::Committed {
        commit: head.stdout.trim().to_owned(),
        paths,
    })
}

/// The explicit path set to commit: each entry with work behind it
/// (`could_not_reproduce == false`) contributes its non-empty `test_file`,
/// deduplicated keeping manifest order.
fn authored_paths(manifest: &AuthoredManifest) -> Vec<String> {
    let mut seen = BTreeSet::new();
    manifest
        .entries
        .iter()
        .filter(|entry| !entry.could_not_reproduce)
        .map(|entry| entry.test_file.trim())
        .filter(|path| !path.is_empty())
        .filter(|path| seen.insert((*path).to_owned()))
        .map(str::to_owned)
        .collect()
}

/// Run one git command in the workspace, mapping an unrunnable command to a
/// contextual message (a non-zero exit is returned as data for the caller).
fn run_git(
    shell: &Shell,
    workspace_path: &str,
    args: &[&str],
    context: &str,
) -> Result<crate::shell::CliRun, String> {
    shell
        .run("git", args, workspace_path)
        .map_err(|failure| format!("{context}: {}", failure.message()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{AuthoredManifest, context_from_input, manifest_from_output};

    #[test]
    fn context_parses_the_test_author_input_wire_shape() {
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
}
