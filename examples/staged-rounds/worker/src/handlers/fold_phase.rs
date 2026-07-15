//! Pure phase-state folding boundary for the AWL-authored round loop.
//!
//! AWL loops thread exactly one value, so this activity partitions the plan's
//! items into ready/blocked/done, applies the round's verdicts under the
//! DERIVE-AND-CHECK rule (any blocking finding rejects; an asserted overall
//! that disagrees is itself a violation treated as reject), attaches
//! reviewer feedback to rejected items for their resumed dev session, and
//! releases newly-unblocked items. It performs no shell I/O and owns no
//! workflow disposition — the loop's `until` reads the returned partitions.

use std::collections::{BTreeMap, BTreeSet};

use aion_worker::ActivityFailure;

use crate::types::{
    DevItemResult, DoneItem, FoldPhaseInput, ItemVerdict, Overall, PhaseState, Severity, WorkItem,
};

/// Fold one completed round (or the plan seed) into the loop-carried
/// [`PhaseState`].
///
/// # Errors
///
/// Terminal on protocol violations that must never pass silently: duplicate
/// item ids, a dev result without a verdict (or vice versa), a dangling
/// `depends_on` id naming no known item, or a dependency deadlock that would
/// leave `blocked` non-empty with `ready` empty.
pub fn fold_phase(input: FoldPhaseInput) -> Result<PhaseState, ActivityFailure> {
    let FoldPhaseInput {
        prior,
        incoming,
        dev,
        verdicts,
    } = input;

    // The working pool: prior ready items plus every newly planned item.
    let mut pool: Vec<WorkItem> = prior.ready;
    pool.extend(incoming);
    let mut blocked = prior.blocked;
    let mut done = prior.done;

    require_unique_ids(&pool, &blocked, &done)?;
    require_known_dependencies(&pool, &blocked, &done)?;

    let mut round_lines: Vec<String> = Vec::new();
    let judged = judge_round(&dev, &verdicts)?;
    for judgement in judged {
        let Some(position) = pool
            .iter()
            .position(|item| item.id == judgement.result.item.id)
        else {
            return Err(ActivityFailure::terminal(format!(
                "dev result for item {} does not correspond to any ready item",
                judgement.result.item.id
            )));
        };
        let mut item = pool.remove(position);
        if judgement.accepted {
            done.push(DoneItem {
                item_id: item.id,
                branch: judgement.result.branch.clone(),
                base_commit: judgement.result.base_commit.clone(),
                summary: judgement.result.report.summary.clone(),
            });
        } else {
            item.feedback.clone_from(&judgement.feedback);
            round_lines.push(format!("item {} rejected: {}", item.id, judgement.feedback));
            pool.push(item);
        }
        round_lines.extend(judgement.violations);
    }

    // Release: every blocked item whose dependencies are all done moves to
    // ready. This preserves the invariant `blocked non-empty ⇒ ready
    // non-empty` for any acyclic dependency graph.
    let done_ids: BTreeSet<&str> = done.iter().map(|entry| entry.item_id.as_str()).collect();
    let (released, still_blocked): (Vec<WorkItem>, Vec<WorkItem>) = blocked
        .drain(..)
        .partition(|item| dependencies_satisfied(item, &done_ids));
    pool.extend(released);
    let (ready, newly_blocked): (Vec<WorkItem>, Vec<WorkItem>) = pool
        .drain(..)
        .partition(|item| dependencies_satisfied(item, &done_ids));
    let mut blocked = still_blocked;
    blocked.extend(newly_blocked);

    if ready.is_empty() && !blocked.is_empty() {
        let stuck: Vec<&str> = blocked.iter().map(|item| item.id.as_str()).collect();
        return Err(ActivityFailure::terminal(format!(
            "dependency deadlock: no item is ready but {stuck:?} remain \
             blocked — the plan's depends_on graph cannot make progress"
        )));
    }

    Ok(PhaseState {
        ready,
        blocked,
        done,
        evidence: smart_join(&prior.evidence, &round_lines.join("; ")),
    })
}

/// One judged dev result: accepted or rejected, with rendered feedback and
/// any derive-and-check violations observed on its verdict.
struct Judgement {
    result: DevItemResult,
    accepted: bool,
    feedback: String,
    violations: Vec<String>,
}

/// Match dev results to verdicts by `item_id` and apply derive-and-check.
/// A dev result with no verdict, a verdict with no dev result, or a
/// duplicated id on either side is a terminal input failure.
fn judge_round(
    dev: &[DevItemResult],
    verdicts: &[ItemVerdict],
) -> Result<Vec<Judgement>, ActivityFailure> {
    let mut by_id: BTreeMap<&str, &ItemVerdict> = BTreeMap::new();
    for verdict in verdicts {
        if by_id.insert(verdict.item_id.as_str(), verdict).is_some() {
            return Err(ActivityFailure::terminal(format!(
                "duplicate verdict for item {}",
                verdict.item_id
            )));
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut judged = Vec::with_capacity(dev.len());
    for result in dev {
        let id = result.item.id.as_str();
        if !seen.insert(id) {
            return Err(ActivityFailure::terminal(format!(
                "duplicate dev result for item {id}"
            )));
        }
        let Some(verdict) = by_id.remove(id) else {
            return Err(ActivityFailure::terminal(format!(
                "dev result for item {id} has no matching verdict"
            )));
        };
        judged.push(judge(result.clone(), verdict));
    }
    if let Some((orphan, _)) = by_id.into_iter().next() {
        return Err(ActivityFailure::terminal(format!(
            "verdict for item {orphan} has no matching dev result"
        )));
    }
    Ok(judged)
}

/// DERIVE-AND-CHECK one verdict: the derived overall is reject iff any
/// finding is blocking. A verdict whose asserted overall disagrees, or a
/// rejection missing its `reject_reason` or a substantiating blocking
/// finding, is treated as REJECT and the violation is recorded.
fn judge(result: DevItemResult, verdict: &ItemVerdict) -> Judgement {
    let has_blocking = verdict
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Blocking);
    let derived = if has_blocking {
        Overall::Reject
    } else {
        Overall::Accept
    };
    let mut violations = Vec::new();
    if verdict.overall != derived {
        violations.push(format!(
            "item {}: asserted overall {:?} disagrees with the derived {:?} — \
             treated as reject",
            verdict.item_id, verdict.overall, derived
        ));
    }
    let rejecting = derived == Overall::Reject || verdict.overall == Overall::Reject;
    let reason_missing = rejecting
        && verdict
            .reject_reason
            .as_deref()
            .is_none_or(|reason| reason.trim().is_empty());
    if reason_missing {
        violations.push(format!(
            "item {}: rejection carries no reject_reason",
            verdict.item_id
        ));
    }
    if verdict.overall == Overall::Reject && !has_blocking {
        violations.push(format!(
            "item {}: rejection carries no substantiating blocking finding",
            verdict.item_id
        ));
    }
    let accepted = !rejecting && violations.is_empty();
    let feedback = if accepted {
        String::new()
    } else {
        render_feedback(verdict)
    };
    Judgement {
        result,
        accepted,
        feedback,
        violations,
    }
}

/// Render a rejected verdict as the feedback line the next round's dev turn
/// reads: the reason plus every blocking finding's title.
fn render_feedback(verdict: &ItemVerdict) -> String {
    let reason = verdict
        .reject_reason
        .as_deref()
        .filter(|reason| !reason.trim().is_empty())
        .unwrap_or("(no reject_reason given)");
    let blocking_titles: Vec<&str> = verdict
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Blocking)
        .map(|finding| finding.title.as_str())
        .collect();
    if blocking_titles.is_empty() {
        reason.to_owned()
    } else {
        format!("{reason} [blocking: {}]", blocking_titles.join("; "))
    }
}

fn dependencies_satisfied(item: &WorkItem, done_ids: &BTreeSet<&str>) -> bool {
    item.depends_on
        .iter()
        .all(|dependency| done_ids.contains(dependency.as_str()))
}

/// Every item id across the partitions must be unique — a duplicate would
/// make verdict matching and dependency release ambiguous.
fn require_unique_ids(
    pool: &[WorkItem],
    blocked: &[WorkItem],
    done: &[DoneItem],
) -> Result<(), ActivityFailure> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let ids = pool
        .iter()
        .map(|item| item.id.as_str())
        .chain(blocked.iter().map(|item| item.id.as_str()))
        .chain(done.iter().map(|entry| entry.item_id.as_str()));
    for id in ids {
        if !seen.insert(id) {
            return Err(ActivityFailure::terminal(format!(
                "duplicate work item id {id} across the phase partitions"
            )));
        }
    }
    Ok(())
}

/// Every `depends_on` id must name a known item (pool, blocked, or done) —
/// a dangling dependency could never release and would deadlock the plan.
fn require_known_dependencies(
    pool: &[WorkItem],
    blocked: &[WorkItem],
    done: &[DoneItem],
) -> Result<(), ActivityFailure> {
    let known: BTreeSet<&str> = pool
        .iter()
        .map(|item| item.id.as_str())
        .chain(blocked.iter().map(|item| item.id.as_str()))
        .chain(done.iter().map(|entry| entry.item_id.as_str()))
        .collect();
    for item in pool.iter().chain(blocked.iter()) {
        for dependency in &item.depends_on {
            if !known.contains(dependency.as_str()) {
                return Err(ActivityFailure::terminal(format!(
                    "item {} depends on {dependency:?}, which names no \
                     planned item",
                    item.id
                )));
            }
        }
    }
    Ok(())
}

fn smart_join(prior_evidence: &str, round_evidence: &str) -> String {
    match (prior_evidence.is_empty(), round_evidence.is_empty()) {
        (_, true) => prior_evidence.to_owned(),
        (true, false) => round_evidence.to_owned(),
        (false, false) => format!("{prior_evidence}; {round_evidence}"),
    }
}
