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

// --- first-round assembly (context only — doctrine is out-of-band) -------

/// Every role renders the context JSON verbatim in a fenced block. The
/// profile is NOT in the prompt — it rides as the session's
/// `--append-system-prompt` text (see `composed_agent_harness`).
#[test]
fn every_role_renders_the_context_json_fenced() {
    for assemble in [super::developer, super::review_lens] {
        let prompt = assemble(CONTEXT);
        assert!(
            prompt.contains(CONTEXT),
            "context present verbatim; prompt was:\n{prompt}"
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
    let prompt = super::developer(tricky);
    assert!(prompt.contains(tricky));
}

// --- gate discipline (task #236) -----------------------------------------

/// The developer prompt carries a hard, prominent gate-discipline
/// section with THIS run's configured gate argv interpolated verbatim —
/// not just buried in the fenced JSON context, but printed out as the
/// exact commands.
#[test]
fn developer_prompt_carries_gate_discipline_with_verbatim_argv() {
    let prompt = super::developer(CONTEXT_FIRST_ROUND);
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
    let prompt = super::developer(CONTEXT_FIRST_ROUND);
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
        let prompt = super::developer(context);
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
    let prompt = super::developer(context);
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
    let prompt = super::developer(CONTEXT);
    assert!(prompt.contains("GATE DISCIPLINE"));
    assert!(prompt.contains("vacuous pass"));
}

/// The gate-discipline section is a DEVELOPER-only addition — the
/// reviewer lens never touches the workspace and never runs gates
/// itself, so its prompt must not carry it.
#[test]
fn review_lens_prompt_has_no_gate_discipline_section() {
    let prompt = super::review_lens(CONTEXT_FIRST_ROUND);
    assert!(
        !prompt.contains("GATE DISCIPLINE"),
        "the review lens does not run gates; it must not get this section"
    );
}

/// The review-lens prompt carries the workspace section: the checkout is
/// the session's working directory, the lens is read-only, and the exact
/// `git diff <base_commit>` command uses the run's real base commit so a
/// truncated diff can be reconstructed.
#[test]
fn review_lens_prompt_carries_the_read_only_workspace_section() {
    let context = "{\"lens\":{\"name\":\"correctness\",\"charter\":\"c\"},\
             \"brief\":{\"id\":\"DB-1\"},\"diff\":\"...\",\
             \"report\":{\"brief_id\":\"DB-1\"},\"gate_runs\":[],\
             \"workspace_path\":\"/repo/.yggdrasil-worktrees/dev-brief/wf-1\",\
             \"base_commit\":\"deadbeefcafe\"}";
    let prompt = super::review_lens(context);
    assert!(
        prompt.contains("YOUR WORKSPACE"),
        "the lens must be told its cwd is the checkout; prompt was:\n{prompt}"
    );
    assert!(
        prompt.contains("READ-ONLY"),
        "the lens must be told it is read-only; prompt was:\n{prompt}"
    );
    assert!(
        prompt.contains("git diff deadbeefcafe"),
        "the exact git diff command must carry the run's real base commit; \
             prompt was:\n{prompt}"
    );
    assert!(
        prompt.contains("TRUNCATED"),
        "the lens must be told a clipped diff is partial, not complete; \
             prompt was:\n{prompt}"
    );
}

/// A lens context missing `base_commit` degrades to the `<base_commit>`
/// placeholder rather than failing the prompt composer.
#[test]
fn review_lens_workspace_section_degrades_without_a_base_commit() {
    let prompt = super::review_lens(CONTEXT);
    assert!(prompt.contains("YOUR WORKSPACE"));
    assert!(
        prompt.contains("git diff <base_commit>"),
        "a missing base_commit renders the placeholder; prompt was:\n{prompt}"
    );
}

// --- loop-back rendering (module-doc exception 2) -------------------------

/// A loop-back round is COMPACT: no raw context JSON dump — the adverse
/// evidence is rendered, formatted, instead. (The doctrine never rode in
/// the prompt at all; it is the session's `--append-system-prompt` text.)
#[test]
fn loop_back_omits_the_raw_context_dump() {
    let prompt = super::developer(CONTEXT_LOOP_BACK);
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
    let prompt = super::developer(CONTEXT_LOOP_BACK);
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
    let prompt = super::developer(CONTEXT_REVIEW_LOOP_BACK);
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
    let prompt = super::developer(context);
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
    let prompt = super::developer(malformed);
    assert!(
        prompt.contains(malformed),
        "the fallback must carry the context verbatim; prompt was:\n{prompt}"
    );
    assert!(
        prompt.contains("GATE DISCIPLINE"),
        "the full-render fallback still carries the gate-discipline \
             section; prompt was:\n{prompt}"
    );
}

/// An object-shaped gate can be projectable but still malformed (here its
/// required `pass` field is absent). Fallback must use the original bytes, not
/// the evidence-stripped intermediate value.
#[test]
fn object_shaped_malformed_gate_preserves_all_original_evidence() {
    let malformed = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":{\"runs\":[{\
             \"name\":\"fmt\",\"exit_code\":0,\"passed\":true,\
             \"output_tail\":\"ORIGINAL_PASSING_TAIL\"}],\
             \"diff\":\"ORIGINAL_DIFF\",\"diagnostics\":\"ORIGINAL_DIAGNOSTICS\"},\
             \"verdicts\":[],\"gates\":[]}";
    let prompt = super::developer(malformed);

    assert!(
        prompt.contains(malformed),
        "fallback changed the input bytes"
    );
    for evidence in [
        "ORIGINAL_PASSING_TAIL",
        "ORIGINAL_DIFF",
        "ORIGINAL_DIAGNOSTICS",
    ] {
        assert!(
            prompt.contains(evidence),
            "fallback lost `{evidence}`; prompt was:\n{prompt}"
        );
    }
}

/// The already-projected Gleam shape is a fixed point, while raw AWL gate
/// output and run records produce the exact same final prompt.
#[test]
fn gate_projection_is_idempotent_and_raw_shape_matches_current_prompt() {
    let current = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":{\"pass\":false,\
             \"runs\":[{\"name\":\"fmt\",\"argv\":[\"cargo\",\"fmt\"],\"exit_code\":0,\
             \"passed\":true,\"output_tail\":\"\"},{\"name\":\"clippy\",\
             \"argv\":[\"cargo\",\"clippy\"],\"exit_code\":101,\"passed\":false,\
             \"output_tail\":\"warning: denied lint\"}],\"diff\":\"\",\"diagnostics\":\"\"},\
             \"verdicts\":[],\"workspace_path\":\"/tmp/ws\",\"gates\":[\
             {\"name\":\"fmt\",\"argv\":[\"cargo\",\"fmt\"]},{\"name\":\"clippy\",\
             \"argv\":[\"cargo\",\"clippy\"]}]}";
    let raw = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":{\"pass\":false,\
             \"runs\":[{\"name\":\"fmt\",\"argv\":[\"cargo\",\"fmt\"],\"exit_code\":0,\
             \"passed\":true,\"output_tail\":\"formatted files\"},{\"name\":\"clippy\",\
             \"argv\":[\"cargo\",\"clippy\"],\"exit_code\":101,\"passed\":false,\
             \"output_tail\":\"warning: denied lint\"}],\"diff\":\"diff --git ...\",\
             \"diagnostics\":\"clippy failed\"},\"verdicts\":[],\
             \"workspace_path\":\"/tmp/ws\",\"gates\":[\
             {\"name\":\"fmt\",\"argv\":[\"cargo\",\"fmt\"],\"exit_code\":0,\
             \"passed\":true,\"output_tail\":\"formatted files\"},{\"name\":\"clippy\",\
             \"argv\":[\"cargo\",\"clippy\"],\"exit_code\":101,\"passed\":false,\
             \"output_tail\":\"warning: denied lint\"}]}";

    assert_eq!(super::project_developer_context(current).as_ref(), current);
    let once = super::project_developer_context(raw).into_owned();
    assert_eq!(super::project_developer_context(&once).as_ref(), once);
    assert_eq!(super::developer(raw), super::developer(current));
}

/// AWL omission and the live Gleam explicit-null optional are one prompt
/// contract, including the first-round verbatim context section.
#[test]
fn absent_gate_renders_exactly_like_explicit_null() {
    let absent = "{\"brief\":{\"id\":\"DB-1\"},\"verdicts\":[],\
             \"workspace_path\":\"/tmp/ws\",\"gates\":[]}";
    let explicit_null = "{\"brief\":{\"id\":\"DB-1\"},\"gate\":null,\"verdicts\":[],\
             \"workspace_path\":\"/tmp/ws\",\"gates\":[]}";

    assert_eq!(
        super::project_developer_context(absent).as_ref(),
        explicit_null
    );
    assert_eq!(super::developer(absent), super::developer(explicit_null));
}
