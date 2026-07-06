#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! REAL-git tests for the test-author commit helper (`commit.rs`): each test
//! initializes an actual temporary repository, seeds a commit, and drives
//! [`commit_authored_tests`] against it — the same `git` binary production
//! uses, no shims. What is asserted is the CONTRACT of the mechanical-git
//! step:
//!
//! - the commit exists, carries ONLY the explicitly-named manifest paths
//!   (stray workspace files stay untracked), and is authored by the scoped
//!   machinery identity;
//! - a manifest claiming a file the agent never wrote is a loud error naming
//!   the path;
//! - an all-`could_not_reproduce` manifest commits nothing and stays green;
//! - a re-run after success skips (activity-retry idempotency), never fails
//!   on an empty commit.

use remediation_worker::commit::{
    AuthoredManifest, CommitOutcome, TEST_AUTHOR_COMMIT_EMAIL, TEST_AUTHOR_COMMIT_NAME,
    commit_authored_tests, manifest_from_output,
};
use remediation_worker::shell::Shell;

/// A real temporary git repository with one seed commit on branch
/// `remediation/B-1` (the brief-branch shape provision creates).
struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = Self { dir };
        repo.git(&["init", "--initial-branch", "remediation/B-1"]);
        // A scoped seed identity distinct from the helper's, so authorship
        // assertions cannot pass by accident.
        std::fs::write(repo.path().join("README.md"), "seed\n").expect("seed file");
        repo.git(&["add", "README.md"]);
        repo.git(&[
            "-c",
            "user.name=seeder",
            "-c",
            "user.email=seeder@example.com",
            "commit",
            "-m",
            "seed",
        ]);
        repo
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn workspace(&self) -> String {
        self.path().display().to_string()
    }

    /// Run git in the repo, asserting success (test setup must never fail
    /// silently).
    fn git(&self, args: &[&str]) -> String {
        let run = Shell::inherited()
            .run("git", args, &self.workspace())
            .expect("git runs");
        assert_eq!(run.exit_status, 0, "git {args:?} failed: {}", run.output);
        run.stdout
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// Write a workspace-relative file, creating parents.
    fn write(&self, relative: &str, contents: &str) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, contents).expect("write");
    }
}

fn manifest(entries: &[(&str, bool)]) -> AuthoredManifest {
    // Build through the production parse path so the projection stays honest
    // to the wire shape.
    let json = serde_json::json!({
        "brief_id": "B-1",
        "entries": entries
            .iter()
            .map(|(test_file, could_not_reproduce)| {
                serde_json::json!({
                    "finding_id": "YG-268",
                    "test_names": ["t"],
                    "test_file": test_file,
                    "expected_failure_signature": "sig",
                    "fail_evidence": "e",
                    "could_not_reproduce": could_not_reproduce,
                    "could_not_reproduce_reason": null,
                    "manual_acceptance": null,
                })
            })
            .collect::<Vec<_>>(),
    });
    manifest_from_output(serde_json::to_vec(&json).expect("encodable").as_slice())
        .expect("manifest parses")
}

#[test]
fn commits_exactly_the_explicit_manifest_paths_under_the_scoped_identity() {
    let repo = Repo::new();
    let seed = repo.head();
    repo.write(
        "tests/yg268_teardown.rs",
        "#[test] fn boom() { panic!() }\n",
    );
    // Stray debris the explicit-path rule must never sweep up.
    repo.write("src/should_not_be_committed.rs", "// stray\n");

    let outcome = commit_authored_tests(
        &Shell::inherited(),
        &repo.workspace(),
        "B-1",
        &manifest(&[("tests/yg268_teardown.rs", false)]),
    )
    .expect("commit succeeds");

    let CommitOutcome::Committed { commit, paths } = outcome else {
        panic!("expected a commit, got {outcome:?}");
    };
    assert_eq!(paths, vec!["tests/yg268_teardown.rs".to_owned()]);
    assert_eq!(commit, repo.head());
    assert_ne!(commit, seed, "a NEW commit must exist");

    // The commit carries ONLY the explicit path.
    let files = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files.trim(), "tests/yg268_teardown.rs");

    // The scoped machinery identity, and the brief id in the message.
    let author = repo.git(&["log", "-1", "--format=%an <%ae>"]);
    assert_eq!(
        author.trim(),
        format!("{TEST_AUTHOR_COMMIT_NAME} <{TEST_AUTHOR_COMMIT_EMAIL}>")
    );
    let message = repo.git(&["log", "-1", "--format=%s"]);
    assert!(
        message.contains("B-1"),
        "the commit message must carry the brief id; was: {message}"
    );

    // The stray file is untouched: still untracked, not in the commit.
    let status = repo.git(&["status", "--porcelain"]);
    assert!(
        status.contains("?? src/"),
        "the stray file must remain untracked; status: {status}"
    );
}

#[test]
fn a_claimed_but_unwritten_test_file_is_a_loud_error_naming_the_path() {
    let repo = Repo::new();
    let seed = repo.head();

    let error = commit_authored_tests(
        &Shell::inherited(),
        &repo.workspace(),
        "B-1",
        &manifest(&[("tests/never_written.rs", false)]),
    )
    .expect_err("must fail");

    assert!(
        error.contains("tests/never_written.rs"),
        "the error must name the missing path; was: {error}"
    );
    assert_eq!(repo.head(), seed, "no commit may be created");
}

#[test]
fn an_all_could_not_reproduce_manifest_skips_and_stays_green() {
    let repo = Repo::new();
    let seed = repo.head();

    let outcome = commit_authored_tests(
        &Shell::inherited(),
        &repo.workspace(),
        "B-1",
        &manifest(&[("", true), ("", true)]),
    )
    .expect("skip is green, never an error");

    assert!(
        matches!(outcome, CommitOutcome::Skipped { .. }),
        "expected a skip, got {outcome:?}"
    );
    assert_eq!(repo.head(), seed, "no commit may be created");
}

#[test]
fn rerunning_after_a_successful_commit_skips_idempotently() {
    let repo = Repo::new();
    repo.write(
        "tests/yg268_teardown.rs",
        "#[test] fn boom() { panic!() }\n",
    );
    let the_manifest = manifest(&[("tests/yg268_teardown.rs", false)]);

    let first = commit_authored_tests(&Shell::inherited(), &repo.workspace(), "B-1", &the_manifest)
        .expect("first run commits");
    assert!(matches!(first, CommitOutcome::Committed { .. }));
    let committed_head = repo.head();

    // An activity retry re-delivers the same manifest against the same
    // workspace: nothing is staged, so the helper must skip, not fail on an
    // empty commit.
    let second =
        commit_authored_tests(&Shell::inherited(), &repo.workspace(), "B-1", &the_manifest)
            .expect("second run is green");
    assert!(
        matches!(second, CommitOutcome::Skipped { .. }),
        "expected a skip, got {second:?}"
    );
    assert_eq!(repo.head(), committed_head, "HEAD must not move");
}

#[test]
fn duplicate_test_file_entries_collapse_to_one_path() {
    let repo = Repo::new();
    repo.write("tests/shared_test.rs", "// two findings, one file\n");

    let outcome = commit_authored_tests(
        &Shell::inherited(),
        &repo.workspace(),
        "B-1",
        &manifest(&[
            ("tests/shared_test.rs", false),
            ("tests/shared_test.rs", false),
        ]),
    )
    .expect("commit succeeds");

    let CommitOutcome::Committed { paths, .. } = outcome else {
        panic!("expected a commit, got {outcome:?}");
    };
    assert_eq!(paths, vec!["tests/shared_test.rs".to_owned()]);
}
