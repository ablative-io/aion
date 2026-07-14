//! Hermetic shell-activity tests for the per-item side: `provision_item`
//! against REAL git in scratch repositories, the pure `fold_phase` boundary,
//! and the destructive-path guard. Runtime behavior is proven by execution —
//! worktrees are read back from disk, never assumed.

use staged_rounds_worker::handlers::{fold_phase, provision_item};
use staged_rounds_worker::paths::guard_destructive_path;
use staged_rounds_worker::shell::Shell;
use staged_rounds_worker::types::{
    DevItemResult, Finding, FoldPhaseInput, ItemReport, ItemVerdict, Overall, PhaseState, Severity,
    WorkItem,
};

mod fixtures;
use fixtures::{commit_file_in, git, item, provision_input, scratch_repo};

// --- local fixtures -------------------------------------------------------

fn item_with_deps(id: &str, phase: i64, depends_on: &[&str]) -> WorkItem {
    WorkItem {
        phase,
        depends_on: depends_on.iter().map(|dep| (*dep).to_owned()).collect(),
        ..item(id)
    }
}

fn empty_state() -> PhaseState {
    PhaseState {
        ready: vec![],
        blocked: vec![],
        done: vec![],
        evidence: String::new(),
    }
}

fn dev_result(work: &WorkItem, branch: &str, base: &str) -> DevItemResult {
    DevItemResult {
        item: work.clone(),
        workspace_path: format!("/ws/{}", work.id),
        branch: branch.to_owned(),
        base_commit: base.to_owned(),
        report: ItemReport {
            item_id: work.id.clone(),
            summary: format!("did {}", work.id),
            commits: vec!["head".to_owned()],
            claims: vec![],
        },
    }
}

fn accept_verdict(id: &str) -> ItemVerdict {
    ItemVerdict {
        item_id: id.to_owned(),
        overall: Overall::Accept,
        findings: vec![],
        reject_reason: None,
    }
}

fn reject_verdict(id: &str, reason: &str, blocking_title: &str) -> ItemVerdict {
    ItemVerdict {
        item_id: id.to_owned(),
        overall: Overall::Reject,
        findings: vec![Finding {
            severity: Severity::Blocking,
            title: blocking_title.to_owned(),
            evidence: "evidence".to_owned(),
        }],
        reject_reason: Some(reason.to_owned()),
    }
}

// --- provision_item -------------------------------------------------------

#[test]
fn provision_item_creates_a_worktree_on_the_item_branch_and_reports_the_base() -> anyhow::Result<()>
{
    let (_dir, repo, run_root) = scratch_repo()?;
    let provisioned = provision_item(
        &Shell::inherited(),
        provision_input(&repo, &run_root, item("it-a")),
    )
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    assert_eq!(provisioned.workspace_path, format!("{run_root}/items/it-a"));
    assert_eq!(provisioned.branch, "staged/wf-1/it-a");
    let head = git(&provisioned.workspace_path, &["rev-parse", "HEAD"])?;
    assert_eq!(provisioned.base_commit, head);
    let branch = git(
        &provisioned.workspace_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )?;
    assert_eq!(branch, "staged/wf-1/it-a");
    Ok(())
}

#[test]
fn provision_item_reuses_a_clean_existing_worktree_preserving_commits() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let shell = Shell::inherited();
    let first = provision_item(&shell, provision_input(&repo, &run_root, item("it-a")))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    commit_file_in(&first.workspace_path, "it-a.txt", "round 1\n", "round 1")?;
    let round1_head = git(&first.workspace_path, &["rev-parse", "HEAD"])?;

    let second = provision_item(&shell, provision_input(&repo, &run_root, item("it-a")))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    assert_eq!(second.workspace_path, first.workspace_path);
    assert_eq!(second.branch, first.branch);
    // The committed round-1 work SURVIVES re-provisioning.
    let head_after = git(&second.workspace_path, &["rev-parse", "HEAD"])?;
    assert_eq!(head_after, round1_head, "round-1 commit must be preserved");
    // And the base is the merge base with the base branch, not the new head.
    let merge_base = git(&repo, &["merge-base", &second.branch, "main"])?;
    assert_eq!(second.base_commit, merge_base);
    assert_ne!(second.base_commit, round1_head);
    Ok(())
}

#[test]
fn provision_item_refuses_a_dirty_existing_worktree() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let shell = Shell::inherited();
    let first = provision_item(&shell, provision_input(&repo, &run_root, item("it-a")))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    std::fs::write(
        std::path::Path::new(&first.workspace_path).join("uncommitted.txt"),
        "dirty\n",
    )?;

    let Err(error) = provision_item(&shell, provision_input(&repo, &run_root, item("it-a"))) else {
        anyhow::bail!("a dirty existing worktree was silently re-provisioned");
    };
    assert!(
        error.message().contains("UNCOMMITTED"),
        "{}",
        error.message()
    );
    Ok(())
}

#[test]
fn provision_item_refuses_a_dirty_stale_holder_by_name() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let shell = Shell::inherited();
    // A stale worktree elsewhere holds the item branch, dirty.
    let stale = format!("{repo}/.staged-rounds/stale-holder");
    git(
        &repo,
        &["worktree", "add", "-B", "staged/wf-1/it-a", &stale, "main"],
    )?;
    std::fs::write(std::path::Path::new(&stale).join("dirty.txt"), "x\n")?;

    let Err(error) = provision_item(&shell, provision_input(&repo, &run_root, item("it-a"))) else {
        anyhow::bail!("a dirty stale holder was silently destroyed");
    };
    assert!(
        error.message().contains("stale-holder"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("UNCOMMITTED"),
        "{}",
        error.message()
    );
    Ok(())
}

#[test]
fn provision_item_reclaims_a_clean_stale_holder() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let shell = Shell::inherited();
    let stale = format!("{repo}/.staged-rounds/stale-holder");
    git(
        &repo,
        &["worktree", "add", "-B", "staged/wf-1/it-a", &stale, "main"],
    )?;

    let provisioned = provision_item(&shell, provision_input(&repo, &run_root, item("it-a")))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    assert_eq!(provisioned.branch, "staged/wf-1/it-a");
    assert!(!std::path::Path::new(&stale).exists());
    Ok(())
}

#[test]
fn provision_item_refuses_a_non_slug_item_id() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    for bad in ["It-A", "it_a", "it a", "", "-a", "a-"] {
        let Err(error) = provision_item(
            &Shell::inherited(),
            provision_input(&repo, &run_root, item(bad)),
        ) else {
            anyhow::bail!("non-slug id {bad:?} was accepted");
        };
        assert!(
            error.message().contains("git-ref-safe slug"),
            "{}",
            error.message()
        );
    }
    Ok(())
}

// --- fold_phase -----------------------------------------------------------

#[test]
fn fold_phase_seeds_ready_and_blocked_from_incoming_by_phase_and_deps() -> anyhow::Result<()> {
    let state = fold_phase(FoldPhaseInput {
        prior: empty_state(),
        incoming: vec![
            item_with_deps("it-a", 1, &[]),
            item_with_deps("it-b", 1, &[]),
            item_with_deps("it-c", 2, &["it-a"]),
        ],
        dev: vec![],
        verdicts: vec![],
    })
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    let ready: Vec<&str> = state.ready.iter().map(|entry| entry.id.as_str()).collect();
    let blocked: Vec<&str> = state
        .blocked
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ready, vec!["it-a", "it-b"]);
    assert_eq!(blocked, vec!["it-c"]);
    assert!(state.done.is_empty());
    Ok(())
}

#[test]
fn fold_phase_accepts_a_clean_verdict_into_done_and_releases_dependents() -> anyhow::Result<()> {
    let seeded = fold_phase(FoldPhaseInput {
        prior: empty_state(),
        incoming: vec![
            item_with_deps("it-a", 1, &[]),
            item_with_deps("it-c", 2, &["it-a"]),
        ],
        dev: vec![],
        verdicts: vec![],
    })
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    let work = seeded.ready[0].clone();
    let state = fold_phase(FoldPhaseInput {
        prior: seeded,
        incoming: vec![],
        dev: vec![dev_result(&work, "staged/wf-1/it-a", "base")],
        verdicts: vec![accept_verdict("it-a")],
    })
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    assert_eq!(state.done.len(), 1);
    assert_eq!(state.done[0].item_id, "it-a");
    assert_eq!(state.done[0].branch, "staged/wf-1/it-a");
    assert_eq!(state.done[0].summary, "did it-a");
    let ready: Vec<&str> = state.ready.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ready, vec!["it-c"], "the dependent must be released");
    assert!(state.blocked.is_empty());
    Ok(())
}

#[test]
fn fold_phase_keeps_a_rejected_item_pending_with_feedback_attached() -> anyhow::Result<()> {
    let work = item("it-a");
    let state = fold_phase(FoldPhaseInput {
        prior: PhaseState {
            ready: vec![work.clone()],
            ..empty_state()
        },
        incoming: vec![],
        dev: vec![dev_result(&work, "staged/wf-1/it-a", "base")],
        verdicts: vec![reject_verdict(
            "it-a",
            "wrong seam",
            "Splits the wrong module",
        )],
    })
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    assert!(state.done.is_empty());
    assert_eq!(state.ready.len(), 1);
    assert_eq!(state.ready[0].id, "it-a");
    assert_eq!(
        state.ready[0].feedback,
        "wrong seam [blocking: Splits the wrong module]"
    );
    assert!(
        state.evidence.contains("it-a rejected"),
        "{}",
        state.evidence
    );
    Ok(())
}

#[test]
fn fold_phase_derives_reject_from_a_blocking_finding_despite_asserted_accept() -> anyhow::Result<()>
{
    let work = item("it-a");
    let mut lying = reject_verdict("it-a", "real problem", "Broken invariant");
    lying.overall = Overall::Accept; // asserted accept, blocking finding
    let state = fold_phase(FoldPhaseInput {
        prior: PhaseState {
            ready: vec![work.clone()],
            ..empty_state()
        },
        incoming: vec![],
        dev: vec![dev_result(&work, "b", "c")],
        verdicts: vec![lying],
    })
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    assert!(state.done.is_empty(), "derive-and-check must reject");
    assert_eq!(state.ready[0].id, "it-a");
    assert!(
        state.evidence.contains("disagrees with the derived"),
        "{}",
        state.evidence
    );
    Ok(())
}

#[test]
fn fold_phase_fails_loudly_on_a_verdict_with_no_matching_dev_result() -> anyhow::Result<()> {
    let work = item("it-a");
    let Err(error) = fold_phase(FoldPhaseInput {
        prior: PhaseState {
            ready: vec![work.clone()],
            ..empty_state()
        },
        incoming: vec![],
        dev: vec![dev_result(&work, "b", "c")],
        verdicts: vec![accept_verdict("it-a"), accept_verdict("it-ghost")],
    }) else {
        anyhow::bail!("an orphan verdict was silently ignored");
    };
    assert!(error.message().contains("it-ghost"), "{}", error.message());

    let Err(error) = fold_phase(FoldPhaseInput {
        prior: PhaseState {
            ready: vec![item("it-a")],
            ..empty_state()
        },
        incoming: vec![],
        dev: vec![dev_result(&item("it-a"), "b", "c")],
        verdicts: vec![],
    }) else {
        anyhow::bail!("a dev result without a verdict was silently ignored");
    };
    assert!(
        error.message().contains("no matching verdict"),
        "{}",
        error.message()
    );
    Ok(())
}

#[test]
fn fold_phase_fails_loudly_on_a_dangling_dependency() -> anyhow::Result<()> {
    let Err(error) = fold_phase(FoldPhaseInput {
        prior: empty_state(),
        incoming: vec![item_with_deps("it-a", 1, &["it-nowhere"])],
        dev: vec![],
        verdicts: vec![],
    }) else {
        anyhow::bail!("a dangling depends_on was accepted");
    };
    assert!(
        error.message().contains("it-nowhere"),
        "{}",
        error.message()
    );
    Ok(())
}

#[test]
fn fold_phase_never_leaves_blocked_nonempty_with_ready_empty() -> anyhow::Result<()> {
    // A dependency cycle can never release: the fold must fail loudly
    // rather than return a state the loop would spin on forever.
    let Err(error) = fold_phase(FoldPhaseInput {
        prior: empty_state(),
        incoming: vec![
            item_with_deps("it-a", 1, &["it-b"]),
            item_with_deps("it-b", 1, &["it-a"]),
        ],
        dev: vec![],
        verdicts: vec![],
    }) else {
        anyhow::bail!("a dependency cycle produced a spinnable state");
    };
    assert!(error.message().contains("deadlock"), "{}", error.message());
    Ok(())
}

// --- the destructive-path guard ---------------------------------------------

#[test]
fn destructive_paths_outside_staged_rounds_root_are_refused() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    std::fs::create_dir_all(&run_root)?;
    // The repo root itself and the staged-rounds root are refused; a real
    // run workspace under it is allowed.
    assert!(guard_destructive_path(&repo, &repo).is_err());
    let staged_root = format!("{repo}/.staged-rounds");
    assert!(guard_destructive_path(&repo, &staged_root).is_err());
    guard_destructive_path(&repo, &run_root)
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    Ok(())
}
