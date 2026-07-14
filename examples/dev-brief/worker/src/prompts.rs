//! Per-role prompt assembly: the per-run CONTEXT block carrying the activity's
//! structured input JSON. The role's profile markdown is NOT part of the
//! prompt — it is the role's STATIC system prompt, passed to Norn once per run
//! via `--append-system-prompt` (which APPENDS to Norn's own system
//! instructions rather than overwriting them; see `composed_agent_harness` in
//! `main.rs`). These functions therefore assemble context only; the doctrine
//! reaches the agent out-of-band as system-prompt text, not folded into the
//! per-turn user message.
//!
//! DELIBERATELY SEPARATED AND DUMB (the remediation-worker discipline, kept):
//! each role has exactly one obvious function whose body IS the interface. No
//! templating engine, no shared cleverness beyond the one context-section
//! renderer they all read as.
//!
//! The context JSON is normally the workflow's activity input verbatim. THREE
//! narrow, deliberate exceptions keep the current Gleam driver and its AWL
//! successor on one worker:
//!
//! 1. Developer gate feedback is projected here before rendering: full raw
//!    `run_gates` output is reduced to failing evidence, while an already
//!    projected Gleam payload is left byte-identical.
//! 2. The developer's gate-discipline section (task #236, below): pulls the
//!    `gates` array back out of that same JSON to print the exact argv a
//!    second time, prominently, in prose.
//! 3. The developer's LOOP-BACK rendering (2026-07-09): the developer runs in
//!    a RESUMED norn session (`--session-id {workflow_id}-developer` +
//!    `--resume-if-exists`), so on every loop-back the role doctrine (the
//!    `--append-system-prompt` text, which persists for the session), the
//!    full brief, and round 1's context are ALREADY in the conversation.
//!    Re-rendering the full context JSON each round buried the one thing the
//!    round exists for — the new adverse evidence — under kilobytes of
//!    repetition. A loop-back round (a prior gate outcome and/or lens verdicts
//!    present in the context) therefore renders a COMPACT prompt: what failed,
//!    formatted for reading, plus the standing gate-discipline section. A
//!    loop-back-shaped context that fails to parse falls back to the full
//!    verbatim render — degradation is verbose, never lossy.
//!
//! # Why the gate-discipline section exists (task #236)
//!
//! Every dev-brief run on 2026-07-06/07 (AWL-1 through AWL-4) burned fix
//! cycles on this workspace's pedantic clippy wall because the developer
//! never ran the brief's own gate commands in its workspace before ending a
//! turn — each cycle it fixed the previously reported lints while writing
//! fresh ones the pipeline's own `run_gates` activity then had to discover,
//! costing a full loop-back every time. [`developer`] renders the exact
//! configured gate argv into a hard, prominent section — on the first round
//! AND on every loop-back — so running it is the obvious last step of every
//! turn, not an optional confidence check.

use std::borrow::Cow;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::types::GateCommand;

/// The signature every role's assembly function shares:
/// `(context_json) -> per-turn prompt`. The profile is NOT a parameter — it
/// rides out-of-band as the session's `--append-system-prompt` text.
pub type AssembleFn = fn(&str) -> String;

/// The one renderer: a titled run-context section carrying the input JSON in a
/// fenced block. The doctrine is the session's system prompt, not part of this
/// text.
fn context_section(context_title: &str, context_json: &str) -> String {
    format!(
        "## {context_title}\n\nThe JSON below is this run's structured context. \
         Work from these structured artifacts — never from a prose summary of \
         them.\n\n```json\n{context_json}\n```\n"
    )
}

/// Project either driver shape to the established developer prompt shape.
///
/// The live Gleam workflow has already blanked `diff`, `diagnostics`, and the
/// `output_tail` of passing runs, and has projected `gates` to `{name, argv}`
/// records. That shape is borrowed unchanged. The AWL successor may pass the
/// raw `run_gates` result (including run records under `gates`), so this
/// boundary performs those pure projections before prompt assembly. A missing
/// optional `gate` is materialized as `null`, matching the live wire shape.
pub(crate) fn project_developer_context(context_json: &str) -> Cow<'_, str> {
    let Ok(Value::Object(mut context)) = serde_json::from_str(context_json) else {
        return Cow::Borrowed(context_json);
    };

    let mut changed = context.get_mut("gate").is_some_and(project_gate_feedback);
    changed |= project_gate_commands(&mut context);

    if changed {
        context.entry("gate").or_insert(Value::Null);
        return serde_json::to_string(&context)
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed(context_json));
    }
    if !context.contains_key("gate")
        && let Some(projected) = insert_absent_gate(context_json)
    {
        return Cow::Owned(projected);
    }
    Cow::Borrowed(context_json)
}

/// Strip the raw gate fields the resumed developer does not need. Returning a
/// change bit makes the projection idempotent without reserializing an already
/// projected payload.
fn project_gate_feedback(gate: &mut Value) -> bool {
    let Value::Object(fields) = gate else {
        return false;
    };
    let mut changed = blank_nonempty_string(fields, "diff");
    changed |= blank_nonempty_string(fields, "diagnostics");
    if let Some(Value::Array(runs)) = fields.get_mut("runs") {
        for run in runs {
            let Value::Object(run_fields) = run else {
                continue;
            };
            if run_fields.get("passed") == Some(&Value::Bool(true)) {
                changed |= blank_nonempty_string(run_fields, "output_tail");
            }
        }
    }
    changed
}

fn blank_nonempty_string(fields: &mut Map<String, Value>, key: &str) -> bool {
    let Some(Value::String(text)) = fields.get_mut(key) else {
        return false;
    };
    if text.is_empty() {
        return false;
    }
    text.clear();
    true
}

/// Project raw gate runs to the configured-command slice consumed by the
/// standing gate-discipline section. The AWL driver may supply either a run
/// list or the containing outcome; the current `{name, argv}` list is stable.
fn project_gate_commands(context: &mut Map<String, Value>) -> bool {
    let source = context
        .get("gates")
        .or_else(|| context.get("gate"))
        .and_then(gate_run_list);
    let Some(projected) = source.and_then(project_commands) else {
        return false;
    };
    if context.get("gates") == Some(&projected) {
        return false;
    }
    context.insert("gates".to_owned(), projected);
    true
}

fn gate_run_list(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Array(runs) => Some(runs),
        Value::Object(fields) => fields
            .get("runs")
            .and_then(Value::as_array)
            .map(Vec::as_slice),
        _ => None,
    }
}

fn project_commands(runs: &[Value]) -> Option<Value> {
    runs.iter()
        .map(|run| {
            let fields = run.as_object()?;
            let mut command = Map::new();
            command.insert("name".to_owned(), fields.get("name")?.clone());
            command.insert("argv".to_owned(), fields.get("argv")?.clone());
            Some(Value::Object(command))
        })
        .collect::<Option<Vec<_>>>()
        .map(Value::Array)
}

/// Insert the AWL absent optional immediately before the required `verdicts`
/// member. This preserves every byte of the first-round context around the one
/// wire-level difference, so it renders exactly like the equivalent Gleam
/// payload containing `"gate":null`.
fn insert_absent_gate(context_json: &str) -> Option<String> {
    let offset = top_level_key_offset(context_json, "verdicts")?;
    let mut projected = String::with_capacity(context_json.len() + 12);
    projected.push_str(&context_json[..offset]);
    projected.push_str("\"gate\":null,");
    projected.push_str(&context_json[offset..]);
    Some(projected)
}

fn top_level_key_offset(json: &str, target: &str) -> Option<usize> {
    let bytes = json.as_bytes();
    let mut depth = 0_u32;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'"' => {
                let start = index;
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => break,
                        _ => index += 1,
                    }
                }
                let end = index.saturating_add(1).min(bytes.len());
                let is_target = depth == 1
                    && serde_json::from_str::<String>(&json[start..end])
                        .is_ok_and(|key| key == target)
                    && json[end..].trim_start().starts_with(':');
                if is_target {
                    return Some(start);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// The slice of the developer's context JSON the gate-discipline section
/// reads: `gates`, this run's configured battery (`config.gates`, the same
/// argv `run_gates` itself executes), carried on EVERY round —
/// [`decode.optional_field`] on the Gleam side means an older/foreign payload
/// missing the key decodes to an empty list here too, never a parse failure.
#[derive(Debug, Deserialize)]
struct DeveloperGates {
    #[serde(default)]
    gates: Vec<GateCommand>,
}

/// Pull `gates` back out of the developer's context JSON — tolerantly: a
/// context that fails to parse or omits the field renders as an empty
/// battery (the discipline section then says so explicitly) rather than
/// panicking the prompt composer over a rendering concern.
fn configured_gates(context_json: &str) -> Vec<GateCommand> {
    serde_json::from_str::<DeveloperGates>(context_json)
        .map(|parsed| parsed.gates)
        .unwrap_or_default()
}

/// The hard, prominent gate-discipline section appended after the developer's
/// run context (see the module doc for why it exists: task #236). Lists this
/// run's configured gate argv VERBATIM, in prose, a second time — the
/// developer should never have to infer the exact command from a name.
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
         Before you end ANY turn — the first round and every loop-back — you \
         MUST run every command below, VERBATIM, in your workspace, and keep \
         fixing and re-running until every one of them is fully clean. This \
         run's configured gate battery, in order:\n\n\
         {commands}\n\n\
         The pipeline runs this SAME battery again after your turn ends. \
         That run is CONFIRMATION that your turn is clean, never your first \
         attempt at discovering whether it is. A turn that ends with a dirty \
         gate has already burned a fix cycle — the next round starts by \
         re-fixing what you could have caught yourself.\n\n\
         Concrete failure modes that have burned fix cycles on this \
         workspace's pedantic clippy wall — none of these are visible from a \
         read of the diff, only from the command's own output: \
         `expect_used` / `panic` denied even inside test code, \
         `manual_let_else`, `match_same_arms`, \
         `semicolon_if_nothing_returned`, `needless_pass_by_value`. Reading \
         the code harder does not catch these. Running the command does.\n"
    )
}

// --- the developer's loop-back rendering (module-doc exception 3) ---------------

/// The slice of the developer's context the loop-back renderer reads. Field
/// names mirror `codecs.developer_input_codec` on the Gleam side; extra
/// fields are ignored. A context whose `gate`/`verdicts` do not match this
/// shape fails the parse as a whole and the caller falls back to the full
/// verbatim render.
#[derive(Debug, Deserialize)]
struct DeveloperContext {
    #[serde(default)]
    brief: Option<BriefIdentitySlice>,
    #[serde(default)]
    gate: Option<GateFeedback>,
    #[serde(default)]
    verdicts: Vec<VerdictSlice>,
}

/// The one brief field the loop-back header names.
#[derive(Debug, Deserialize)]
struct BriefIdentitySlice {
    #[serde(default)]
    id: String,
}

/// A prior gate outcome (the `gate_outcome_codec` wire shape; `diff` and
/// `diagnostics` are deliberately not read — the workflow strips them for
/// developer feedback, and the renderer would ignore them anyway).
#[derive(Debug, Deserialize)]
struct GateFeedback {
    pass: bool,
    #[serde(default)]
    runs: Vec<GateRunSlice>,
}

/// One gate command's recorded run.
#[derive(Debug, Deserialize)]
struct GateRunSlice {
    name: String,
    exit_code: i64,
    passed: bool,
    #[serde(default)]
    output_tail: String,
}

/// One lens verdict (the `lens_verdict_codec` wire shape).
#[derive(Debug, Deserialize)]
struct VerdictSlice {
    lens: String,
    #[serde(default)]
    findings: Vec<FindingSlice>,
    overall: String,
    #[serde(default)]
    reject_reason: Option<String>,
}

/// One lens finding.
#[derive(Debug, Deserialize)]
struct FindingSlice {
    severity: String,
    title: String,
    evidence: String,
}

impl VerdictSlice {
    /// Whether this verdict loops the developer back (anything not an
    /// explicit accept is adverse — unknown tags are never a silent pass).
    fn is_adverse(&self) -> bool {
        !self.overall.eq_ignore_ascii_case("accept")
    }
}

/// Outcome of parsing a projected context for compact loop-back rendering.
enum LoopBackPrompt {
    /// A valid loop-back rendered compactly from projected evidence.
    Compact(String),
    /// A valid first round that should render the projected full context.
    FirstRound,
    /// A malformed context that must render the original, unprojected bytes.
    Malformed,
}

/// Render a valid loop-back compactly while distinguishing a valid first round
/// from malformed input. That distinction is load-bearing: projection may have
/// stripped raw gate evidence, but malformed fallback remains byte-lossless by
/// rendering the caller's original input.
fn loop_back_prompt(context_json: &str) -> LoopBackPrompt {
    let Ok(context) = serde_json::from_str::<DeveloperContext>(context_json) else {
        return LoopBackPrompt::Malformed;
    };
    if context.gate.is_none() && context.verdicts.is_empty() {
        return LoopBackPrompt::FirstRound;
    }
    let brief_id = context
        .brief
        .as_ref()
        .map_or("(unknown)", |brief| brief.id.as_str());

    let mut prompt = format!(
        "## Loop-back round — brief {brief_id}\n\n\
         This message continues YOUR resumed session: the role doctrine, the \
         full brief, and your workspace are unchanged and already in this \
         conversation — nothing is re-attached. Below is ONLY the new \
         adverse evidence from the pipeline. Address it, then run the gate \
         battery before you end the turn.\n"
    );
    if let Some(gate) = &context.gate {
        prompt.push('\n');
        prompt.push_str(&render_gate_feedback(gate));
    }
    if !context.verdicts.is_empty() {
        prompt.push('\n');
        prompt.push_str(&render_verdicts(&context.verdicts));
    }
    prompt.push('\n');
    prompt.push_str(&gate_discipline_section(context_json));
    LoopBackPrompt::Compact(prompt)
}

/// The gate battery's outcome, formatted: one verdict line per command, then
/// each FAILING command's captured output in its own fenced block.
fn render_gate_feedback(gate: &GateFeedback) -> String {
    let failed = gate.runs.iter().filter(|run| !run.passed).count();
    let headline = if gate.pass {
        format!(
            "### Gate battery: all {} command(s) green\n\n\
             (This loop-back was caused by review verdicts, not the gates — \
             see below.)\n",
            gate.runs.len()
        )
    } else {
        format!(
            "### Gate battery: {failed} of {} command(s) FAILED\n",
            gate.runs.len()
        )
    };
    let mut parts = vec![headline, "\n".to_owned()];
    for run in &gate.runs {
        parts.push(if run.passed {
            format!("- `{}` — passed\n", run.name)
        } else {
            format!("- `{}` — FAILED (exit {})\n", run.name, run.exit_code)
        });
    }
    for run in gate.runs.iter().filter(|run| !run.passed) {
        parts.push(format!(
            "\n#### `{}` output (exit {})\n\n",
            run.name, run.exit_code
        ));
        let tail = run.output_tail.trim();
        parts.push(if tail.is_empty() {
            "(no output captured)\n".to_owned()
        } else {
            fenced_text(tail)
        });
    }
    parts.concat()
}

/// The review verdicts, formatted: accepted lenses as one line each, every
/// adverse verdict expanded with its reason and findings.
fn render_verdicts(verdicts: &[VerdictSlice]) -> String {
    let adverse = verdicts
        .iter()
        .filter(|verdict| verdict.is_adverse())
        .count();
    let mut parts = vec![format!(
        "### Review verdicts: {adverse} of {} lens(es) adverse\n\n",
        verdicts.len()
    )];
    for verdict in verdicts.iter().filter(|verdict| !verdict.is_adverse()) {
        parts.push(format!("- `{}` — accepted\n", verdict.lens));
    }
    for verdict in verdicts.iter().filter(|verdict| verdict.is_adverse()) {
        parts.push(format!(
            "\n#### Lens `{}` — {}\n\n",
            verdict.lens,
            verdict.overall.to_uppercase()
        ));
        parts.push(match &verdict.reject_reason {
            Some(reason) if !reason.trim().is_empty() => {
                format!("Reason: {}\n", reason.trim())
            }
            _ => "Reason: (no reject_reason given)\n".to_owned(),
        });
        if verdict.findings.is_empty() {
            parts.push("Findings: (none listed)\n".to_owned());
        } else {
            parts.push("Findings:\n".to_owned());
            for finding in &verdict.findings {
                parts.push(format!(
                    "- [{}] {}\n  {}\n",
                    finding.severity,
                    finding.title,
                    finding.evidence.trim().replace('\n', "\n  ")
                ));
            }
        }
    }
    parts.concat()
}

/// Fence arbitrary captured output as a text block, choosing a fence longer
/// than any backtick run inside it so an embedded triple-backtick fence can
/// never break out of the block.
fn fenced_text(text: &str) -> String {
    let longest_backtick_run = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest_backtick_run.max(3) + 1);
    format!("{fence}text\n{text}\n{fence}\n")
}

// --- the role assembly functions ------------------------------------------------

/// The developer prompt (context only — the doctrine is the session's
/// `--append-system-prompt` text). FIRST round: the full context JSON fenced
/// verbatim, then the gate-discipline section (task #236). LOOP-BACK rounds (a
/// prior gate outcome and/or verdicts in the context): the compact formatted
/// rendering — the resumed session already holds the doctrine and the brief
/// (module-doc exception 2); the discipline section still rides on every round.
#[must_use]
pub fn developer(context_json: &str) -> String {
    let projected = project_developer_context(context_json);
    let render_context = match loop_back_prompt(projected.as_ref()) {
        LoopBackPrompt::Compact(rendered) => return rendered,
        LoopBackPrompt::FirstRound => projected.as_ref(),
        LoopBackPrompt::Malformed => context_json,
    };
    let prompt = context_section(
        "Run context: brief (+ gate/verdict feedback when cycling)",
        render_context,
    );
    let gate_discipline = gate_discipline_section(render_context);
    format!("{prompt}\n{gate_discipline}")
}

/// The slice of the lens context the workspace section reads: `base_commit`,
/// the provisioned base the parent's diff was taken against — printed so a
/// lens can copy the exact `git diff <sha>` command. Tolerant: a context
/// missing the key renders the generic `<base_commit>` placeholder rather
/// than failing the prompt composer.
#[derive(Debug, Deserialize)]
struct ReviewerContext {
    #[serde(default)]
    base_commit: String,
}

/// The reviewer workspace section, appended after the lens run context. It
/// states the standing fact the reviewer role now depends on: the session's
/// working directory IS the run's checkout at the EXACT reviewed state, the
/// lens is READ-ONLY (its file-mutating tools are denied at the process
/// boundary), and — because the `diff` in the context is clipped — a lens that
/// sees a truncation marker must reconstruct the full change with `git diff`
/// against the base commit itself.
fn reviewer_workspace_section(context_json: &str) -> String {
    let base_commit = serde_json::from_str::<ReviewerContext>(context_json)
        .map(|parsed| parsed.base_commit)
        .unwrap_or_default();
    let base = if base_commit.trim().is_empty() {
        "<base_commit>".to_owned()
    } else {
        base_commit
    };
    format!(
        "## YOUR WORKSPACE — the checkout under review\n\n\
         Your working directory IS the run's git checkout at the EXACT state \
         you are reviewing (the developer's work and any gate normalization \
         are committed; the tree is clean). You are READ-ONLY: your \
         file-writing tools are disabled — read, grep, and run `git` freely, \
         but do not attempt to modify anything.\n\n\
         The `diff` in your run context above is CLIPPED to keep the durable \
         record small. If it shows a truncation marker (`bytes TRUNCATED`), it \
         is a PARTIAL capture, not the whole change — do not mistake \
         `not shown` for `not changed`. Reconstruct the full diff yourself, in \
         your workspace:\n\n\
         ```\n\
         git diff {base}\n\
         ```\n\n\
         and read any file at its reviewed state directly. Ground every \
         finding in what the code and the real diff actually say.\n"
    )
}

/// The review-lens prompt (context only — the doctrine is the session's
/// `--append-system-prompt` text): {lens charter + brief + diff + dev report +
/// gate evidence}, followed by the workspace section (the lens is rooted,
/// read-only, at the run's checkout and can reconstruct a truncated diff with
/// `git diff <base_commit>`).
#[must_use]
pub fn review_lens(context_json: &str) -> String {
    let context = context_section(
        "Run context: lens charter + brief + diff + dev report + gate evidence",
        context_json,
    );
    let workspace = reviewer_workspace_section(context_json);
    format!("{context}\n{workspace}")
}

#[cfg(test)]
mod tests;
