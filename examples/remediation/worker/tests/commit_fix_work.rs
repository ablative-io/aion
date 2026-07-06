#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! REAL-git tests for the developer commit helper (`commit.rs`
//! `commit_fix_work`): each test initializes an actual temporary repository,
//! seeds tracked files, and drives the helper — the same `git` binary
//! production uses, no shims. What is asserted is the CONTRACT of the
//! mechanical fix commit:
//!
//! - `git add --update` stages tracked modifications/deletions only —
//!   untracked debris NOT named by the report stays untracked;
//! - report-named new test files (path claims, containing `/`) enter the
//!   commit explicitly; name-shaped `new_tests` entries (`module::case`) are
//!   ignored, never treated as missing files;
//! - the commit is authored by the scoped `remediation-developer` identity,
//!   its subject carries the brief id, and its body records the staged set;
//! - a path claim the agent never wrote is a loud error naming it;
//! - a no-change round skips green with the unchanged head; a retry after a
//!   successful commit skips idempotently — and BOTH outcomes surface the
//!   real branch head (`FixCommitOutcome::head`), the hash the activity
//!   result's `commits` is rewritten to;
//! - `rewrite_report_commits` replaces the agent's fabricated hashes with
//!   that real head.

use remediation_worker::commit::{
    DEVELOPER_COMMIT_EMAIL, DEVELOPER_COMMIT_NAME, FixCommitOutcome, FixReportSlice,
    commit_fix_work, fix_report_from_output, rewrite_report_commits,
};
use remediation_worker::shell::Shell;

/// A real temporary git repository with tracked seed files on branch
/// `remediation/B-1` (the brief-branch shape provision creates, tests
/// already committed as gate 1 requires).
struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = Self { dir };
        repo.git(&["init", "--initial-branch", "remediation/B-1"]);
        repo.write("src/lib.rs", "pub fn broken() {}\n");
        repo.write("tests/yg268_teardown.rs", "#[test] fn boom() {}\n");
        repo.git(&["add", "src/lib.rs", "tests/yg268_teardown.rs"]);
        repo.git(&[
            "-c",
            "user.name=seeder",
            "-c",
            "user.email=seeder@example.com",
            "commit",
            "-m",
            "seed: tests committed",
        ]);
        repo
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn workspace(&self) -> String {
        self.path().display().to_string()
    }

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

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, contents).expect("write");
    }
}

/// Build the report slice through the production parse path so the
/// projection stays honest to the wire shape.
fn report(new_tests: &[&str]) -> FixReportSlice {
    let json = serde_json::json!({
        "brief_id": "B-1",
        "commits": ["agent-fabricated-hash"],
        "findings_addressed": [{"finding_id": "YG-268", "how": "guarded"}],
        "findings_bounced": [],
        "deviations": [],
        "new_tests": new_tests,
        "class_instances_found": [],
    });
    fix_report_from_output(serde_json::to_vec(&json).expect("encodable").as_slice())
        .expect("report parses")
}

#[test]
fn commits_the_tracked_delta_and_report_named_new_files_under_the_scoped_identity() {
    let repo = Repo::new();
    let seed = repo.head();
    // The developer's work: a tracked modification, a report-named new test
    // file, and stray debris the report does NOT name.
    repo.write("src/lib.rs", "pub fn fixed() {}\n");
    repo.write("tests/extra_guard.rs", "#[test] fn guard() {}\n");
    repo.write("scratch/notes.txt", "debris\n");

    let outcome = commit_fix_work(
        &Shell::inherited(),
        &repo.workspace(),
        "B-1",
        // A name-shaped entry rides along and must be ignored, not treated
        // as a missing file.
        &report(&["tests/extra_guard.rs", "teardown::extra_case"]),
    )
    .expect("commit succeeds");

    let FixCommitOutcome::Committed { commit, paths } = &outcome else {
        panic!("expected a commit, got {outcome:?}");
    };
    assert_eq!(commit, &repo.head());
    assert_ne!(commit, &seed, "a NEW commit must exist");
    assert_eq!(outcome.head(), repo.head(), "head() surfaces the real hash");
    assert_eq!(
        paths,
        &vec!["src/lib.rs".to_owned(), "tests/extra_guard.rs".to_owned()],
        "the staged set is the tracked delta plus the named new file"
    );

    // The commit carries exactly those paths.
    let files = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
    let mut listed: Vec<&str> = files.lines().filter(|line| !line.is_empty()).collect();
    listed.sort_unstable();
    assert_eq!(listed, vec!["src/lib.rs", "tests/extra_guard.rs"]);

    // Scoped identity; brief id in the subject; staged set in the body.
    let author = repo.git(&["log", "-1", "--format=%an <%ae>"]);
    assert_eq!(
        author.trim(),
        format!("{DEVELOPER_COMMIT_NAME} <{DEVELOPER_COMMIT_EMAIL}>")
    );
    let subject = repo.git(&["log", "-1", "--format=%s"]);
    assert!(subject.contains("B-1"), "subject was: {subject}");
    let body = repo.git(&["log", "-1", "--format=%b"]);
    assert!(
        body.contains("src/lib.rs") && body.contains("tests/extra_guard.rs"),
        "the body must record the staged paths; was: {body}"
    );

    // The un-named debris stays untracked — never an untracked sweep.
    let status = repo.git(&["status", "--porcelain"]);
    assert!(
        status.contains("?? scratch/"),
        "unnamed debris must remain untracked; status: {status}"
    );
}

#[test]
fn a_claimed_but_unwritten_new_test_file_is_a_loud_error_naming_the_path() {
    let repo = Repo::new();
    let seed = repo.head();
    repo.write("src/lib.rs", "pub fn fixed() {}\n");

    let error = commit_fix_work(
        &Shell::inherited(),
        &repo.workspace(),
        "B-1",
        &report(&["tests/never_written.rs"]),
    )
    .expect_err("must fail");

    assert!(
        error.contains("tests/never_written.rs"),
        "the error must name the missing path; was: {error}"
    );
    assert_eq!(repo.head(), seed, "no commit may be created");
}

#[test]
fn a_round_that_changed_nothing_skips_green_with_the_unchanged_head() {
    let repo = Repo::new();
    let seed = repo.head();

    let outcome = commit_fix_work(&Shell::inherited(), &repo.workspace(), "B-1", &report(&[]))
        .expect("skip is green, never an error");

    let FixCommitOutcome::Skipped { head, .. } = &outcome else {
        panic!("expected a skip, got {outcome:?}");
    };
    assert_eq!(head, &seed, "the surfaced head is the unchanged branch tip");
    assert_eq!(repo.head(), seed, "no commit may be created");
}

#[test]
fn rerunning_after_a_successful_commit_skips_idempotently_surfacing_the_same_head() {
    let repo = Repo::new();
    repo.write("src/lib.rs", "pub fn fixed() {}\n");
    let the_report = report(&[]);

    let first = commit_fix_work(&Shell::inherited(), &repo.workspace(), "B-1", &the_report)
        .expect("first run commits");
    assert!(matches!(first, FixCommitOutcome::Committed { .. }));
    let committed_head = repo.head();

    let second = commit_fix_work(&Shell::inherited(), &repo.workspace(), "B-1", &the_report)
        .expect("second run is green");
    assert!(
        matches!(second, FixCommitOutcome::Skipped { .. }),
        "expected a skip, got {second:?}"
    );
    assert_eq!(second.head(), committed_head, "same real hash surfaced");
    assert_eq!(repo.head(), committed_head, "HEAD must not move");
}

#[test]
fn the_result_rewrite_carries_the_real_commit_hash_downstream() {
    let repo = Repo::new();
    repo.write("src/lib.rs", "pub fn fixed() {}\n");

    // The agent's raw output asserts a hash it never made (it cannot know
    // one — it never ran git).
    let raw_output = serde_json::to_vec(&serde_json::json!({
        "brief_id": "B-1",
        "commits": ["agent-fabricated-hash"],
        "findings_addressed": [{"finding_id": "YG-268", "how": "guarded"}],
        "findings_bounced": [],
        "deviations": [],
        "new_tests": [],
        "class_instances_found": [],
    }))
    .expect("encodable");

    let outcome = commit_fix_work(
        &Shell::inherited(),
        &repo.workspace(),
        "B-1",
        &fix_report_from_output(&raw_output).expect("parses"),
    )
    .expect("commit succeeds");

    let rewritten = rewrite_report_commits(&raw_output, outcome.head()).expect("rewrites");
    let value: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
    assert_eq!(
        value["commits"],
        serde_json::json!([repo.head()]),
        "downstream (ledger fix_commit, verdict) must see the real hash"
    );
    assert_eq!(
        value["findings_addressed"],
        serde_json::json!([{"finding_id": "YG-268", "how": "guarded"}]),
        "everything else survives untouched"
    );
}
