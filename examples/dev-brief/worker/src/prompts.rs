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
//! for the contract to drift. ONE narrow, deliberate exception: the
//! developer's gate-discipline section (below) pulls the `gates` array back
//! out of that same JSON to print the exact argv a second time, prominently,
//! in prose — the same narrow precedent `commit::context_from_input` already
//! sets for `brief.id` / `workspace_path`. It is additive, not a
//! reprojection: the full context JSON still rides through byte-verbatim in
//! its own fenced block.
//!
//! # Why the gate-discipline section exists (task #236)
//!
//! Every dev-brief run on 2026-07-06/07 (AWL-1 through AWL-4) burned fix
//! cycles on this workspace's pedantic clippy wall because the developer
//! never ran the brief's own gate commands in its workspace before ending a
//! turn — each cycle it fixed the previously reported lints while writing
//! fresh ones the pipeline's own `run_gates` activity then had to discover,
//! costing a full loop-back every time. [`developer`] now renders the exact
//! configured gate argv into a hard, prominent section so running it is the
//! obvious last step of every turn, not an optional confidence check.

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

/// The developer prompt: profile, then {brief, and on loop-back rounds the
/// failing gate outcome and/or the adverse lens verdicts being addressed},
/// then the gate-discipline section (task #236) — the same section on every
/// call, first round or loop-back, since both run through this one function.
#[must_use]
pub fn developer(profile: &str, context_json: &str) -> String {
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
    /// null`), this run's configured battery present under `gates`.
    const CONTEXT_FIRST_ROUND: &str = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":null,\
         \"verdicts\":[],\"workspace_path\":\"/tmp/ws\",\"gates\":[\
         {\"name\":\"fmt\",\"argv\":[\"cargo\",\"fmt\",\"--all\",\"--\",\"--check\"]},\
         {\"name\":\"clippy\",\"argv\":[\"cargo\",\"clippy\",\"--workspace\",\
         \"--all-targets\",\"--\",\"-D\",\"warnings\"]}]}";

    /// A loop-back developer context: a prior FAILING gate outcome rides
    /// alongside the SAME configured `gates` battery — the exact shape a
    /// fix-cycle round sees, since `prompts::developer` is the one function
    /// both the first round and every loop-back call.
    const CONTEXT_LOOP_BACK: &str = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":{\"pass\":false,\
         \"runs\":[{\"name\":\"clippy\",\"exit_code\":101,\"passed\":false,\
         \"output_tail\":\"warning: used `expect`\"}],\"diff\":\"...\",\
         \"diagnostics\":\"clippy failed\"},\"verdicts\":[],\
         \"workspace_path\":\"/tmp/ws\",\"gates\":[\
         {\"name\":\"fmt\",\"argv\":[\"cargo\",\"fmt\",\"--all\",\"--\",\"--check\"]},\
         {\"name\":\"clippy\",\"argv\":[\"cargo\",\"clippy\",\"--workspace\",\
         \"--all-targets\",\"--\",\"-D\",\"warnings\"]}]}";

    /// Every role: profile first, context JSON verbatim after, fenced.
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
    fn context_json_is_never_reprojected() {
        // A context containing text that LOOKS like a field must survive
        // byte-for-byte: assembly is concatenation, not templating.
        let tricky = "{\"detail\":\"contains {workflow_id} and ```\"}";
        let prompt = super::developer(PROFILE, tricky);
        assert!(prompt.contains(tricky));
    }

    /// STANDING CONTRACT (kept from the remediation worker): the profile
    /// markdown reaches the prompt byte-identical from disk — no trimming,
    /// reflowing, or normalization beyond removing TRAILING whitespace.
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

    /// The SAME `developer` function composes both the first round and every
    /// loop-back round (there is no separate loop-back prompt variant), so
    /// the gate-discipline section — and the exact argv — must appear
    /// identically on both: a context with no prior gate outcome, and one
    /// carrying a prior FAILING gate outcome.
    #[test]
    fn gate_discipline_appears_identically_on_first_round_and_loop_back() {
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
}
