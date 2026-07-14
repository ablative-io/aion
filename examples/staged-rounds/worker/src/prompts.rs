//! Per-role prompt assembly: the per-run CONTEXT block carrying the
//! activity's structured input JSON. The role's profile markdown is NOT part
//! of the prompt — it is the role's STATIC system prompt, passed to Norn once
//! per run via `--append-system-prompt` (which APPENDS to Norn's own system
//! instructions rather than overwriting them; see `composed_agent_harness` in
//! `main.rs`). These functions therefore assemble context only; the doctrine
//! reaches the agent out-of-band as system-prompt text, not folded into the
//! per-turn user message.
//!
//! DELIBERATELY SEPARATED AND DUMB (the dev-brief discipline, kept): each
//! role has exactly one obvious function whose body IS the interface. No
//! templating engine, no shared cleverness beyond the one context-section
//! renderer they all read as.
//!
//! # Why the gate-discipline section exists (task #236)
//!
//! Dev-brief runs burned fix cycles on pedantic clippy walls because the
//! developer never ran the brief's own gate commands in its workspace before
//! ending a turn. [`dev_item`] renders the exact configured gate argv into a
//! hard, prominent section so running it is the obvious last step of every
//! turn, not an optional confidence check.

use serde::Deserialize;

use crate::types::GateCommand;

/// The signature every role's assembly function shares:
/// `(context_json) -> per-turn prompt`. The profile is NOT a parameter — it
/// rides out-of-band as the session's `--append-system-prompt` text.
pub type AssembleFn = fn(&str) -> String;

/// The one renderer: a titled run-context section carrying the input JSON in
/// a fenced block. The doctrine is the session's system prompt, not part of
/// this text.
fn context_section(context_title: &str, context_json: &str) -> String {
    format!(
        "## {context_title}\n\nThe JSON below is this run's structured context. \
         Work from these structured artifacts — never from a prose summary of \
         them.\n\n```json\n{context_json}\n```\n"
    )
}

/// The slice of the dev context the gate-discipline section reads: `gates`,
/// this run's configured battery (`config.gates`, the same argv the item
/// profile tells the agent to run). Tolerant: a context that fails to parse
/// or omits the field renders as an empty battery (the discipline section
/// then says so explicitly) rather than panicking the prompt composer over a
/// rendering concern.
#[derive(Debug, Deserialize)]
struct DevGates {
    #[serde(default)]
    gates: Vec<GateCommand>,
}

fn configured_gates(context_json: &str) -> Vec<GateCommand> {
    serde_json::from_str::<DevGates>(context_json)
        .map(|parsed| parsed.gates)
        .unwrap_or_default()
}

/// The hard, prominent gate-discipline section appended after the dev run
/// context (see the module doc for why it exists: task #236). Lists this
/// run's configured gate argv VERBATIM, in prose, a second time — the dev
/// agent should never have to infer the exact command from a name.
fn gate_discipline_section(context_json: &str) -> String {
    let gates = configured_gates(context_json);
    let commands = if gates.is_empty() {
        "(this run's configured gate battery is empty — the operator's \
         explicit choice, a recorded vacuous pass; there is nothing to run, \
         but hold your own work to the same bar the gates would have \
         enforced)"
            .to_owned()
    } else {
        gates
            .iter()
            .map(|gate| format!("- {}: `{}`", gate.name, gate.argv.join(" ")))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "## GATE DISCIPLINE — DO THIS BEFORE YOU FINISH THIS TURN\n\n\
         Before you end ANY turn — the first round and every feedback \
         round — you MUST run every command below, VERBATIM, in your \
         workspace, and keep fixing and re-running until every one of them \
         is fully clean. This run's configured gate battery, in order:\n\n\
         {commands}\n\n\
         The pipeline reviews your work after your turn ends. Its review is \
         CONFIRMATION that your turn is clean, never your first attempt at \
         discovering whether it is. A turn that ends with a dirty gate has \
         already burned a fix cycle — the next round starts by re-fixing \
         what you could have caught yourself.\n"
    )
}

// --- the role assembly functions ---------------------------------------------

/// The planner prompt (context only — the doctrine is the session's
/// `--append-system-prompt` text): the material, the repo root the session
/// is read-only rooted at, and the output charter.
#[must_use]
pub fn planner(context_json: &str) -> String {
    let context = context_section("Run context: material + repo root", context_json);
    format!(
        "{context}\n\
         ## Your job\n\n\
         Read the material and the repository (your working directory IS the \
         repo, read-only — ground the plan in the actual tree). Return the \
         typed plan: 3-10 work items, each with a git-ref-safe slug `id` \
         (lowercase, `[a-z0-9-]`), a one-sentence `goal`, `scope_in` (the \
         files/dirs the item MAY touch), `scope_out` (hard walls), an \
         integer `phase` (1 = no prerequisites), `depends_on` listing the \
         item ids whose MERGED output this item needs, and `feedback: \"\"`.\n\n\
         Items in the same phase run IN PARALLEL in separate worktrees \
         against the same base branch — they MUST NOT touch the same files; \
         use phases and `depends_on` for anything that would collide or \
         build on other items' work.\n\n\
         ## Output\n\n\
         The JSON schema is enforced; emit nothing but the object.\n"
    )
}

/// The dev-item prompt: the item context (including reviewer feedback when
/// cycling) plus the standing gate-discipline section.
#[must_use]
pub fn dev_item(context_json: &str) -> String {
    let context = context_section(
        "Run context: your work item (+ reviewer feedback when cycling)",
        context_json,
    );
    let gate_discipline = gate_discipline_section(context_json);
    format!(
        "{context}\n\
         ## Your workspace and your turn\n\n\
         Your working directory IS this item's own git worktree on its own \
         branch. Implement `work.item.goal` strictly inside `scope_in`, \
         never touching `scope_out` — sibling items are being edited in \
         parallel, and an out-of-fence change manufactures merge conflicts \
         for the whole run. You run NO git: the machinery commits your work \
         after the turn and records the real hash.\n\n\
         If `work.item.feedback` is non-empty, this is your RESUMED session \
         continuing YOUR previous round — the reviewer rejected it for the \
         reasons quoted there; address them first.\n\n\
         {gate_discipline}\n\
         ## Report\n\n\
         One `claims` entry per acceptance-relevant point: concretely HOW \
         the diff meets it. The JSON schema is enforced; emit nothing but \
         the object.\n"
    )
}

/// The slice of the reviewer context the workspace section reads:
/// `work.base_commit`, the provisioned base the item's diff is taken
/// against — printed so the reviewer can copy the exact `git diff <sha>`
/// command. Tolerant: a context missing the key renders the generic
/// `<base_commit>` placeholder rather than failing the prompt composer.
#[derive(Debug, Deserialize)]
struct ReviewerContext {
    #[serde(default)]
    work: ReviewerWork,
}

#[derive(Debug, Default, Deserialize)]
struct ReviewerWork {
    #[serde(default)]
    base_commit: String,
}

/// The reviewer workspace section, appended after the item run context. It
/// states the standing facts the reviewer role depends on: the session's
/// working directory IS the item worktree at the exact reviewed state, the
/// reviewer is READ-ONLY (file-mutating tools denied at the process
/// boundary), and the full diff is reconstructed with `git diff` against
/// the base commit.
fn reviewer_workspace_section(context_json: &str) -> String {
    let base_commit = serde_json::from_str::<ReviewerContext>(context_json)
        .map(|parsed| parsed.work.base_commit)
        .unwrap_or_default();
    let base = if base_commit.trim().is_empty() {
        "<base_commit>".to_owned()
    } else {
        base_commit
    };
    format!(
        "## YOUR WORKSPACE — the worktree under review\n\n\
         Your working directory IS the item's worktree at the EXACT state \
         you are reviewing (the round's work is committed; the tree is \
         clean). You are READ-ONLY: your file-writing tools are disabled — \
         read, grep, and run `git` freely, but do not attempt to modify \
         anything.\n\n\
         Reconstruct the full change yourself, in your workspace:\n\n\
         ```\n\
         git diff {base}\n\
         ```\n\n\
         and read any file at its reviewed state directly. You MAY run the \
         configured gate commands read-only when their output would ground \
         a finding. Ground every finding in what the code and the real diff \
         actually say.\n"
    )
}

/// The review-item prompt: the item + dev report context, followed by the
/// workspace section.
#[must_use]
pub fn review_item(context_json: &str) -> String {
    let context = context_section("Run context: item + dev report", context_json);
    let workspace = reviewer_workspace_section(context_json);
    format!(
        "{context}\n{workspace}\n\
         ## Charter\n\n\
         You are the item's single adversarial reviewer, covering \
         correctness, scope-fence compliance (`scope_out` violations are \
         BLOCKING), and claim verification. Any blocking finding means \
         overall reject with a `reject_reason`. The JSON schema is \
         enforced; emit nothing but the object.\n"
    )
}

/// The remediate prompt: the merge state + the plan, and the resumed-planner
/// framing.
#[must_use]
pub fn remediate(context_json: &str) -> String {
    let context = context_section("Run context: merge state + the plan", context_json);
    format!(
        "{context}\n\
         ## You are the planner, resumed\n\n\
         This session planned these items. A merge of their branches hit \
         the conflicts listed in `merge.conflicts`. Your working directory \
         IS the integration worktree with the conflicted merge IN \
         PROGRESS — the conflict markers are in the tree right now. Resolve \
         every conflict marker, preserving BOTH items' intent per the plan. \
         Do NOT run `git commit` — the machinery concludes the merge after \
         your turn.\n\n\
         ## Output\n\n\
         A summary (what conflicted, whose intent won where, why) plus the \
         list of files you resolved. The JSON schema is enforced; emit \
         nothing but the object.\n"
    )
}

#[cfg(test)]
#[path = "prompts/tests.rs"]
mod tests;
