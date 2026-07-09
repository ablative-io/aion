//! Per-role prompt assembly: {the role's profile markdown} + {the per-run
//! context block carrying the activity's structured input JSON}.
//!
//! DELIBERATELY SEPARATED AND DUMB (the remediation-worker discipline, kept):
//! each role has exactly one obvious function whose body IS the interface. No
//! templating engine, no shared cleverness beyond the one two-section joiner
//! they all read as.
//!
//! The context JSON is the workflow's activity input verbatim; nothing is
//! parsed or re-projected here — a lossy re-rendering would be a second place
//! for the contract to drift. TWO narrow, deliberate exceptions:
//!
//! 1. The developer's gate-discipline section (task #236, below): pulls the
//!    `gates` array back out of that same JSON to print the exact argv a
//!    second time, prominently, in prose — additive, not a reprojection.
//! 2. The developer's LOOP-BACK rendering (2026-07-09): the developer runs in
//!    a RESUMED norn session (`--session-id {workflow_id}-developer` +
//!    `--resume-if-exists`), so on every loop-back the role doctrine, the
//!    full brief, and round 1's context are ALREADY in the conversation.
//!    Re-injecting the profile plus the full context JSON each round buried
//!    the one thing the round exists for — the new adverse evidence — under
//!    kilobytes of repetition (observed on the AWL0-REFAC-001 dispatches,
//!    runs `672b43a4`/`a4b40d8a`). A loop-back round (a prior gate outcome
//!    and/or lens verdicts present in the context) therefore renders a
//!    COMPACT prompt: what failed, formatted for reading, plus the standing
//!    gate-discipline section. A loop-back-shaped context that fails to parse
//!    falls back to the full verbatim render — degradation is verbose, never
//!    lossy.
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

use serde::Deserialize;

use crate::types::GateCommand;

/// The signature every role's assembly function shares:
/// `(profile_markdown, context_json) -> full prompt`.
pub type AssembleFn = fn(&str, &str) -> String;

/// The one joiner: profile first (the doctrine), then a titled run-context
/// section carrying the input JSON in a fenced block.
fn profile_then_context(profile: &str, context_title: &str, context_json: &str) -> String {
    format!(
        "{}\n\n---\n\n## {}\n\nThe JSON below is this run's structured context. \
         Work from these structured artifacts — never from a prose summary of \
         them.\n\n```json\n{}\n```\n",
        profile.trim_end(),
        context_title,
        context_json
    )
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

// --- the developer's loop-back rendering (module-doc exception 2) ---------------

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

/// Render the compact loop-back prompt, or `None` when this is a first round
/// (no prior gate outcome, no verdicts) or the context does not parse as the
/// expected wire shape (the caller then renders the full verbatim form —
/// never lossy). The profile is deliberately absent from the signature: a
/// loop-back never re-injects it, because the resumed session already holds
/// the doctrine.
fn loop_back_prompt(context_json: &str) -> Option<String> {
    let context: DeveloperContext = serde_json::from_str(context_json).ok()?;
    if context.gate.is_none() && context.verdicts.is_empty() {
        return None;
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
    Some(prompt)
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

/// The developer prompt. FIRST round: profile (the doctrine), then the full
/// context JSON fenced verbatim, then the gate-discipline section (task
/// #236). LOOP-BACK rounds (a prior gate outcome and/or verdicts in the
/// context): the compact formatted rendering — the resumed session already
/// holds the doctrine and the brief (module-doc exception 2); the discipline
/// section still rides on every round.
#[must_use]
pub fn developer(profile: &str, context_json: &str) -> String {
    if let Some(rendered) = loop_back_prompt(context_json) {
        return rendered;
    }
    let prompt = profile_then_context(
        profile,
        "Run context: brief (+ gate/verdict feedback when cycling)",
        context_json,
    );
    let gate_discipline = gate_discipline_section(context_json);
    format!("{prompt}\n{gate_discipline}")
}

/// The review-lens prompt: profile, then {lens charter + brief + diff + dev
/// report + gate evidence}.
#[must_use]
pub fn review_lens(profile: &str, context_json: &str) -> String {
    profile_then_context(
        profile,
        "Run context: lens charter + brief + diff + dev report + gate evidence",
        context_json,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    const PROFILE: &str = "# Role\n\nDoctrine body.";
    const CONTEXT: &str = "{\"brief\":{\"id\":\"DB-1\"}}";

    /// A first-round developer context: no prior gate outcome (`"gate":
    /// null`), no verdicts, this run's configured battery present under
    /// `gates`.
    const CONTEXT_FIRST_ROUND: &str = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":null,\
         \"verdicts\":[],\"workspace_path\":\"/tmp/ws\",\"gates\":[\
         {\"name\":\"fmt\",\"argv\":[\"cargo\",\"fmt\",\"--all\",\"--\",\"--check\"]},\
         {\"name\":\"clippy\",\"argv\":[\"cargo\",\"clippy\",\"--workspace\",\
         \"--all-targets\",\"--\",\"-D\",\"warnings\"]}]}";

    /// A loop-back developer context: a prior FAILING gate outcome rides
    /// alongside the SAME configured `gates` battery — the exact shape a
    /// fix-cycle round sees.
    const CONTEXT_LOOP_BACK: &str = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":{\"pass\":false,\
         \"runs\":[{\"name\":\"fmt\",\"exit_code\":0,\"passed\":true,\
         \"output_tail\":\"formatted 3 files\"},\
         {\"name\":\"clippy\",\"exit_code\":101,\"passed\":false,\
         \"output_tail\":\"warning: used `expect`\"}],\"diff\":\"\",\
         \"diagnostics\":\"\"},\"verdicts\":[],\
         \"workspace_path\":\"/tmp/ws\",\"gates\":[\
         {\"name\":\"fmt\",\"argv\":[\"cargo\",\"fmt\",\"--all\",\"--\",\"--check\"]},\
         {\"name\":\"clippy\",\"argv\":[\"cargo\",\"clippy\",\"--workspace\",\
         \"--all-targets\",\"--\",\"-D\",\"warnings\"]}]}";

    /// A review loop-back: gates green, one adverse verdict + one accepting.
    const CONTEXT_REVIEW_LOOP_BACK: &str = "{\"brief\":{\"id\":\"DB-1\"},\
         \"gate\":{\"pass\":true,\"runs\":[{\"name\":\"clippy\",\"exit_code\":0,\
         \"passed\":true,\"output_tail\":\"\"}],\"diff\":\"\",\"diagnostics\":\"\"},\
         \"verdicts\":[\
         {\"lens\":\"correctness\",\"findings\":[],\"overall\":\"accept\",\
         \"reject_reason\":null},\
         {\"lens\":\"spec_fidelity\",\"findings\":[{\"severity\":\"blocking\",\
         \"title\":\"R2 span drift\",\"evidence\":\"line 40 offset wrong\"}],\
         \"overall\":\"reject\",\"reject_reason\":\"R2 not honored\"}],\
         \"workspace_path\":\"/tmp/ws\",\"gates\":[\
         {\"name\":\"clippy\",\"argv\":[\"cargo\",\"clippy\"]}]}";

    // --- first-round assembly (unchanged contract) ---------------------------

    /// Every role: profile first, context JSON verbatim after, fenced —
    /// first rounds and the review lens keep the original contract.
    #[test]
    fn every_role_assembles_profile_then_context() {
        for assemble in [super::developer, super::review_lens] {
            let prompt = assemble(PROFILE, CONTEXT);
            let profile_at = prompt.find("Doctrine body.").expect("profile present");
            let context_at = prompt.find(CONTEXT).expect("context present verbatim");
            assert!(
                profile_at < context_at,
                "the profile must precede the context"
            );
            assert!(
                prompt.contains("```json"),
                "context rides in a fenced block"
            );
        }
    }

    #[test]
    fn first_round_context_json_is_never_reprojected() {
        // A first-round context containing text that LOOKS like a field must
        // survive byte-for-byte: assembly is concatenation, not templating.
        let tricky = "{\"detail\":\"contains {workflow_id} and ```\"}";
        let prompt = super::developer(PROFILE, tricky);
        assert!(prompt.contains(tricky));
    }

    /// STANDING CONTRACT (kept from the remediation worker): the profile
    /// markdown reaches first-round prompts byte-identical from disk — no
    /// trimming, reflowing, or normalization beyond removing TRAILING
    /// whitespace.
    #[test]
    fn profile_markdown_survives_byte_identical_up_to_trailing_trim() {
        let odd_profile = "# Role   with   odd   spacing\n\
                           \n\
                           \t- a tab-indented bullet\n\
                           line one  \n\
                           line two\twith\ttabs\n\
                           \n\
                           ```rust\n\
                           fn keep_me() {}   // trailing spaces inside fences  \n\
                           ```\n\
                           \n\
                           em-dash — and «unicode» stay\n\n\n";
        for assemble in [super::developer, super::review_lens] {
            let prompt = assemble(odd_profile, "{}");
            let expected = odd_profile.trim_end();
            assert!(
                prompt.starts_with(expected),
                "the profile must open the prompt byte-identical (up to the \
                 trailing-whitespace trim); prompt was:\n{prompt}"
            );
            assert!(
                prompt[expected.len()..].starts_with("\n\n---\n\n"),
                "the separator must follow the untouched profile"
            );
        }
    }

    // --- gate discipline (task #236) -----------------------------------------

    /// The developer prompt carries a hard, prominent gate-discipline
    /// section with THIS run's configured gate argv interpolated verbatim —
    /// not just buried in the fenced JSON context, but printed out as the
    /// exact commands.
    #[test]
    fn developer_prompt_carries_gate_discipline_with_verbatim_argv() {
        let prompt = super::developer(PROFILE, CONTEXT_FIRST_ROUND);
        assert!(
            prompt.contains("GATE DISCIPLINE"),
            "must carry a prominent gate-discipline heading; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("cargo fmt --all -- --check"),
            "the fmt gate's exact argv must be interpolated verbatim"
        );
        assert!(
            prompt.contains("cargo clippy --workspace --all-targets -- -D warnings"),
            "the clippy gate's exact argv must be interpolated verbatim"
        );
        assert!(
            prompt.contains("MUST run every command below, VERBATIM"),
            "the instruction must be unambiguous, not a suggestion"
        );
        assert!(
            prompt.contains("CONFIRMATION"),
            "must state the pipeline's own gate battery is confirmation, not discovery"
        );
    }

    /// The concrete failure modes actually observed burning fix cycles
    /// (AWL-1..AWL-4, task #236) are named so the developer knows exactly
    /// what a pedantic clippy wall can deny that a diff read will not catch.
    #[test]
    fn developer_prompt_names_the_observed_pedantic_clippy_failure_modes() {
        let prompt = super::developer(PROFILE, CONTEXT_FIRST_ROUND);
        for failure_mode in [
            "expect_used",
            "panic",
            "manual_let_else",
            "match_same_arms",
            "semicolon_if_nothing_returned",
            "needless_pass_by_value",
        ] {
            assert!(
                prompt.contains(failure_mode),
                "must name the observed failure mode `{failure_mode}`; prompt was:\n{prompt}"
            );
        }
    }

    /// The gate-discipline section — and the exact argv — appears on the
    /// first round AND on every loop-back (the loop-back render appends the
    /// same section).
    #[test]
    fn gate_discipline_appears_on_first_round_and_loop_back() {
        for context in [CONTEXT_FIRST_ROUND, CONTEXT_LOOP_BACK] {
            let prompt = super::developer(PROFILE, context);
            assert!(
                prompt.contains("GATE DISCIPLINE"),
                "gate discipline must appear whether or not this is a loop-back; \
                 prompt was:\n{prompt}"
            );
            assert!(
                prompt.contains("cargo fmt --all -- --check"),
                "the exact argv must appear regardless of round"
            );
            assert!(
                prompt.contains("cargo clippy --workspace --all-targets -- -D warnings"),
                "the exact argv must appear regardless of round"
            );
        }
    }

    /// An explicit EMPTY gate battery (the operator's documented vacuous
    /// pass, mirrored from `run_gates`) still gets a discipline section —
    /// never silently dropped — naming the battery as empty rather than
    /// listing commands that do not exist.
    #[test]
    fn empty_gate_battery_still_renders_a_discipline_section() {
        let context = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":null,\"verdicts\":[],\
             \"workspace_path\":\"/tmp/ws\",\"gates\":[]}";
        let prompt = super::developer(PROFILE, context);
        assert!(prompt.contains("GATE DISCIPLINE"));
        assert!(
            prompt.contains("vacuous pass"),
            "an empty battery must be named as the operator's explicit choice, \
             not silently omitted; prompt was:\n{prompt}"
        );
    }

    /// A context missing the `gates` key entirely (an older/foreign payload)
    /// degrades to an empty battery rather than a parse failure — the
    /// composer must never panic over a rendering concern.
    #[test]
    fn missing_gates_key_degrades_to_an_empty_battery() {
        let prompt = super::developer(PROFILE, CONTEXT);
        assert!(prompt.contains("GATE DISCIPLINE"));
        assert!(prompt.contains("vacuous pass"));
    }

    /// The gate-discipline section is a DEVELOPER-only addition — the
    /// reviewer lens never touches the workspace and never runs gates
    /// itself, so its prompt must not carry it.
    #[test]
    fn review_lens_prompt_has_no_gate_discipline_section() {
        let prompt = super::review_lens(PROFILE, CONTEXT_FIRST_ROUND);
        assert!(
            !prompt.contains("GATE DISCIPLINE"),
            "the review lens does not run gates; it must not get this section"
        );
    }

    // --- loop-back rendering (module-doc exception 2) -------------------------

    /// A loop-back round is COMPACT: no profile re-injection (the resumed
    /// session already holds the doctrine) and no raw context JSON dump —
    /// the adverse evidence is rendered, formatted, instead.
    #[test]
    fn loop_back_omits_the_profile_and_the_raw_context_dump() {
        let prompt = super::developer(PROFILE, CONTEXT_LOOP_BACK);
        assert!(
            !prompt.contains("Doctrine body."),
            "the profile must not be re-injected into a resumed session; \
             prompt was:\n{prompt}"
        );
        assert!(
            !prompt.contains("output_tail"),
            "the raw context JSON must not be dumped on loop-backs; \
             prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("Loop-back round — brief DB-1"),
            "the loop-back must name itself and the brief; prompt was:\n{prompt}"
        );
    }

    /// Failing gates render a verdict line and their captured output in a
    /// fenced block; passing gates render ONLY the verdict line — their
    /// output is noise the round does not need.
    #[test]
    fn loop_back_renders_failing_gate_output_and_mutes_passing_output() {
        let prompt = super::developer(PROFILE, CONTEXT_LOOP_BACK);
        assert!(
            prompt.contains("1 of 2 command(s) FAILED"),
            "the battery headline must count the reds; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("- `clippy` — FAILED (exit 101)"),
            "the failing gate must carry its exit code; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("warning: used `expect`"),
            "the failing gate's output must be present; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("- `fmt` — passed"),
            "the passing gate must render its verdict line; prompt was:\n{prompt}"
        );
        assert!(
            !prompt.contains("formatted 3 files"),
            "a passing gate's output is noise and must not render; \
             prompt was:\n{prompt}"
        );
    }

    /// A review loop-back (gates green, adverse verdicts) expands every
    /// adverse verdict with its reason and findings, and reduces accepting
    /// lenses to one line.
    #[test]
    fn review_loop_back_expands_adverse_verdicts_only() {
        let prompt = super::developer(PROFILE, CONTEXT_REVIEW_LOOP_BACK);
        assert!(
            prompt.contains("1 of 2 lens(es) adverse"),
            "the verdict headline must count the adverse; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("Lens `spec_fidelity` — REJECT"),
            "the adverse verdict must be expanded; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("Reason: R2 not honored"),
            "the reject_reason must render; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("- [blocking] R2 span drift"),
            "findings must render with severity and title; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("- `correctness` — accepted"),
            "accepting lenses reduce to one line; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("caused by review verdicts"),
            "a green battery on a review loop-back must say why it is shown; \
             prompt was:\n{prompt}"
        );
    }

    /// Captured gate output containing triple-backtick fences cannot break
    /// out of the rendering: the fence is always longer than any backtick
    /// run inside.
    #[test]
    fn loop_back_fences_survive_embedded_backticks() {
        let context = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":{\"pass\":false,\
             \"runs\":[{\"name\":\"test\",\"exit_code\":1,\"passed\":false,\
             \"output_tail\":\"before\\n```\\ninjected\\n```\\nafter\"}],\
             \"diff\":\"\",\"diagnostics\":\"\"},\"verdicts\":[],\"gates\":[]}";
        let prompt = super::developer(PROFILE, context);
        assert!(
            prompt.contains("````text"),
            "the fence must outgrow the embedded backtick runs; \
             prompt was:\n{prompt}"
        );
        assert!(prompt.contains("injected"));
    }

    /// A loop-back-shaped context that does NOT parse as the expected wire
    /// shape falls back to the full verbatim render — degradation is
    /// verbose, never lossy.
    #[test]
    fn malformed_loop_back_context_falls_back_to_the_verbatim_render() {
        let malformed = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":\"bogus — not an object\",\
             \"verdicts\":[],\"gates\":[]}";
        let prompt = super::developer(PROFILE, malformed);
        assert!(
            prompt.contains("Doctrine body."),
            "the fallback must render the profile; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains(malformed),
            "the fallback must carry the context verbatim; prompt was:\n{prompt}"
        );
    }
}
