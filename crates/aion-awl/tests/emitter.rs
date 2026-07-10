//! Integration tests for rev-2 AWL-to-Gleam emission.
//!
//! Three layers: a corpus-wide sweep (every valid rev-2 fixture must emit),
//! structural regressions carried over from the panel-hardened AWL-0 emitter
//! (keyword sanitization, string-name child spawn, language-owned loop
//! counters, determinism denylist), and real `gleam build` compile proofs
//! over the flagship and the loop/fork-heavy fixtures (#248 provenance
//! protocol applies to those under parallel load).

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl::{check_in, emit, emit_in, parse};

fn rev2_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rev2")
}

fn emitted_fixture(relative: &str) -> Result<String, Box<dyn Error>> {
    let path = rev2_root().join(relative);
    let source = fs::read_to_string(&path)?;
    let document = parse(&source)?;
    let dir = path
        .parent()
        .ok_or("fixture path has no parent directory")?;
    Ok(emit_in(&document, dir)?)
}

fn emitted(source: &str) -> Result<String, Box<dyn Error>> {
    Ok(emit(&parse(source)?)?)
}

/// Every valid rev-2 fixture parses, checks, and emits: the corpus is the
/// phase gate, and the emitter refuses nothing the checker accepts.
#[test]
fn every_valid_fixture_emits() -> Result<(), Box<dyn Error>> {
    let mut swept = 0;
    for family in fs::read_dir(rev2_root())? {
        let family = family?.path();
        let valid = family.join("valid");
        if !valid.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&valid)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("awl") {
                continue;
            }
            let source = fs::read_to_string(&path)?;
            let document =
                parse(&source).map_err(|error| format!("{}: parse: {error}", path.display()))?;
            let errors = check_in(&document, &valid);
            assert!(
                errors.is_empty(),
                "{}: check errors: {:?}",
                path.display(),
                errors
                    .iter()
                    .map(|error| error.message.clone())
                    .collect::<Vec<_>>()
            );
            emit_in(&document, &valid)
                .map_err(|error| format!("{}: emit: {error}", path.display()))?;
            swept += 1;
        }
    }
    assert!(swept >= 45, "expected the full corpus, swept {swept}");
    Ok(())
}

/// AWL identifiers that are Gleam reserved words gain a trailing underscore
/// at every emission site, while wire-format JSON keys keep the AWL name
/// (panel-hardened AWL-0 behavior, carried over).
#[test]
fn gleam_keywords_are_sanitized_everywhere() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Keyword stress.
workflow keywords
  input let: String
  outcome done: type Shout, route success

type Shout { use: String }

worker keywords
  action greet(fn: String) -> Shout

step greet
  greet(fn: let) -> use

  outcome ok: otherwise, route done(use: use.use)
";
    let generated = emitted(source)?;
    assert!(
        generated.contains("let let_ = input.let_"),
        "input binding and field access must sanitize: {generated}"
    );
    assert!(generated.contains("let_: String,"), "input record field");
    assert!(
        generated.contains("use let_ <- decode.field(\"let\", string_decoder())"),
        "decoder binding keeps the AWL wire name"
    );
    assert!(generated.contains("fn_: String,"), "action param field");
    assert!(
        generated.contains("use use_ <- try(greet_activity(let_)"),
        "sanitized binding and reference at the call site: {generated}"
    );
    assert!(
        generated.contains("#(\"use\", string_to_json(inner))")
            || generated.contains("#(\"use\", string_to_json(value.use_))"),
        "JSON key keeps the AWL name: {generated}"
    );
    Ok(())
}

/// The loop counter is language-owned: maintained in generated code, never
/// threaded through worker results (survey fix 1 / D3).
#[test]
fn loop_counter_is_language_owned() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("loop-outcomes/valid/loop_counting_until_max.awl")?;
    assert!(
        generated.contains("let awl_count = awl_count + 1"),
        "the generated loop must increment its own counter: {generated}"
    );
    assert!(
        generated.contains("use #(status, polls) <- try(poll_loop_0("),
        "the loop must return the threaded value and the counter: {generated}"
    );
    assert!(
        generated.contains("case awl_count >= awl_max {"),
        "the ceiling must gate recursion: {generated}"
    );
    assert!(
        generated.contains("poll_activity(job_id, status)"),
        "the worker dispatch must carry only the declared params, never the counter: {generated}"
    );
    // The counter reaches the outcome payloads from the loop's return alone.
    assert!(generated.contains("Finished(job_id: job_id, polls: polls)"));
    Ok(())
}

/// Child calls keep the string-name spawn discipline: registration-name
/// spawn, JSON-object input, a witness fn the SDK never calls, and no
/// phantom child module references.
#[test]
fn child_call_lowers_to_string_name_spawn() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("declarations/valid/child_call_awaited.awl")?;
    assert!(generated.contains("workflow.spawn_and_wait(\"score_essay\""));
    assert!(
        generated.contains("json.object([#(\"essay\", string_to_json(essay))])"),
        "named child args must encode as one JSON object: {generated}"
    );
    assert!(generated.contains(
        "fn(_: json.Json) { Error(AwlChildFailed(\"child workflow body runs in its own \
         execution\")) }"
    ));
    assert!(!generated.contains("score_essay.execute"));
    assert!(!generated.contains("score_essay.input_codec"));
    Ok(())
}

/// `spawn` is detached: the SDK spawn is used, the handle is discarded, and
/// the workflow continues.
#[test]
fn spawn_lowers_detached() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("declarations/valid/spawn_detached.awl")?;
    assert!(
        generated.contains("use _ <- try(workflow.spawn(\"audit_trail\""),
        "spawn must be detached and unbound: {generated}"
    );
    assert!(
        generated.contains("map_spawn_error"),
        "a failed detached spawn is a step failure: {generated}"
    );
    assert!(!generated.contains("spawn_and_wait(\"audit_trail\""));
    Ok(())
}

/// Emitted modules never call the SDK's nondeterministic surface — time and
/// randomness are engine-mediated through the constructs the language has.
#[test]
fn emitted_sources_avoid_nondeterministic_apis() -> Result<(), Box<dyn Error>> {
    for fixture in [
        "flagship/valid/dev_brief.awl",
        "loop-outcomes/valid/ship_release_combined.awl",
        "dag-fork/valid/release_pipeline_combined.awl",
    ] {
        let generated = emitted_fixture(fixture)?;
        for denied in ["workflow.now", "workflow.random", "erlang/", "os."] {
            assert!(
                !generated.contains(denied),
                "{fixture}: generated source contains forbidden API {denied}"
            );
        }
    }
    Ok(())
}

/// The `on failure` compensation runs only in the error arm, ends in its
/// route, and the step's outcomes stay on the success path.
#[test]
fn on_failure_compensates_then_routes() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("loop-outcomes/valid/on_failure_compensation.awl")?;
    let error_arm = generated
        .find("Error(_) -> {")
        .ok_or("compensation arm missing")?;
    let after = &generated[error_arm..];
    assert!(
        after.contains("delete_assets_activity(assets)"),
        "compensation must run in the error arm: {generated}"
    );
    assert!(
        after.contains("Error(AwlOutcomeFailure(\"failed\""),
        "compensation must end in its declared failure route: {generated}"
    );
    let success_outcome = generated
        .find(
            "Error(AwlOutcomeFailure(\"failed\", json.to_string(publish_failed_to_json(\
                PublishFailed(reason: \"nothing was published\")))))",
        )
        .ok_or("the step's own failure outcome must remain reachable")?;
    let _ = success_outcome;
    Ok(())
}

/// Fork over a collection keeps SDK `map` semantics (exactly-once per item,
/// input order); the `sequential` form is an ordered fold.
#[test]
fn fork_forms_map_and_fold() -> Result<(), Box<dyn Error>> {
    let parallel = emitted_fixture("dag-fork/valid/fork_collection_join.awl")?;
    assert!(
        parallel.contains("use verdicts <- try(workflow.map(docs, fn(doc) {"),
        "collection fork must ride workflow.map: {parallel}"
    );
    let sequential = emitted_fixture("dag-fork/valid/fork_sequential.awl")?;
    assert!(
        sequential.contains("list.try_fold(migrations, [], fn(awl_acc, migration)"),
        "sequential fork must fold in input order: {sequential}"
    );
    assert!(
        sequential.contains("let receipts = list.reverse(awl_folded)"),
        "fold results must join in input order: {sequential}"
    );
    Ok(())
}

/// A `wait … timeout` binding is optional: expiry yields `None`, arrival
/// yields `Some`, and the outcome guard unwraps under `is present`.
#[test]
fn wait_timeout_binds_optionally() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("loop-outcomes/valid/guard_optional_wait.awl")?;
    assert!(generated.contains("Ok(value) -> Ok(Some(value))"));
    assert!(generated.contains("Error(error.TimedOutError(_)) -> Ok(None)"));
    assert!(
        generated.contains("case option.is_some(reply) {"),
        "the guard must test presence: {generated}"
    );
    assert!(
        generated.contains("let assert Some(reply) = reply"),
        "the arm must unwrap the guarded optional: {generated}"
    );
    Ok(())
}

/// Enum-total outcome clauses lower to one exhaustive Gleam `case` over the
/// subject instead of a guard cascade.
#[test]
fn enum_total_outcomes_lower_to_case() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("loop-outcomes/valid/enum_when_totality.awl")?;
    assert!(
        generated.contains("case result.category {"),
        "the subject must scrutinize directly: {generated}"
    );
    for variant in ["Urgent -> {", "Routine -> {", "Spam -> {"] {
        assert!(
            generated.contains(variant),
            "missing arm {variant}: {generated}"
        );
    }
    Ok(())
}

/// A heterogeneous named-branch fork dispatches every branch on ONE
/// `workflow.all` (spec: "bare `fork` is parallel"): the branches ride raw
/// wire-unified activities and the join decodes each payload with its
/// action's return codec (2026-07-11 emitter panel, blocking finding).
#[test]
fn heterogeneous_named_fork_dispatches_one_parallel_all() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("dag-fork/valid/fork_named_branches.awl")?;
    let all_line = generated
        .lines()
        .find(|line| line.contains("workflow.all(["))
        .ok_or("the fork must ride workflow.all")?;
    assert!(
        all_line.contains("fetch_profile_activity_raw(user_id)")
            && all_line.contains("fetch_history_activity_raw(user_id)"),
        "both branches must dispatch in the same workflow.all: {all_line}"
    );
    assert!(
        generated.contains("use profile <- try(awl_decoded(profile_codec(), awl_raw_0,"),
        "branch 1 must decode with its return codec: {generated}"
    );
    assert!(
        generated.contains("use history <- try(awl_decoded(history_codec(), awl_raw_1,"),
        "branch 2 must decode with its return codec: {generated}"
    );
    Ok(())
}

/// Steps sharing a dependency and not each other run in parallel (the
/// flagship's `scout`/`warm_build` layer): a heterogeneous single-call layer is
/// one `workflow.all`, never one-at-a-time dispatch (2026-07-11 emitter
/// panel, blocking finding).
#[test]
fn flagship_scout_and_warm_build_share_one_parallel_all() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("flagship/valid/dev_brief.awl")?;
    let all_line = generated
        .lines()
        .find(|line| line.contains("workflow.all([") && line.contains("scout_activity_raw("))
        .ok_or("scout must dispatch through workflow.all")?;
    assert!(
        all_line.contains("warm_build_activity_raw("),
        "warm_build must dispatch in the same workflow.all as scout: {all_line}"
    );
    assert!(
        generated.contains("use scout_report <- try(awl_decoded(scout_report_codec(), awl_raw_0,"),
        "the bound branch must decode with its return codec: {generated}"
    );
    Ok(())
}

/// A dependency layer whose members are more than single bare calls cannot
/// dispatch concurrently in the stopgap; the sequential fallback announces
/// itself in the generated module instead of degrading silently.
#[test]
fn sequential_layer_fallback_is_announced() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("dag-fork/valid/release_pipeline_combined.awl")?;
    assert!(
        generated.contains("// awl stopgap: these dependency-parallel steps run in written order"),
        "the sequentialized layer must be named in the output: {generated}"
    );
    Ok(())
}

/// A `route` inside a `loop` body is a language-level check error since the
/// 2026-07-11 ruling (loops exit via `until`/`max`; routing belongs to the
/// step's outcome clauses) — the user-facing refusal lives in `check`. The
/// emitter keeps this spanned refusal as the defensive backstop for `emit`
/// called on an UNCHECKED document, where the route could never reach tail
/// position in the generated loop function (originally a 2026-07-11 emitter
/// panel blocking finding).
#[test]
fn route_inside_a_loop_body_is_refused() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Loop route stress.
workflow loop_route
  input target: String
  outcome done: type Report, route success
  outcome bailed: type Report, route failure

type Round  { summary: String, gates_green: Bool }
type Report { summary: String }

worker builder
  action work(prior: Round) -> Round

step build
  loop round = Round(summary: \"\", gates_green: false) counting cycles
    work(prior: round) -> round
    route bailed(summary: round.summary)
    until round.gates_green
    max 3

  outcome green: when round.gates_green, route done(summary: round.summary)
  outcome spent: otherwise, route done(summary: \"spent\")
";
    let document = parse(source)?;
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("`route` inside a `loop` body"),
        "unexpected message: {}",
        error.message
    );
    let route_line = source
        .lines()
        .position(|line| line.contains("route bailed"))
        .ok_or("missing route line")?
        + 1;
    assert_eq!(error.span.line, route_line, "span must anchor the route");
    Ok(())
}

/// A loop `max` referencing the loop-threaded value renders at the call
/// site, where no loop-local exists: the stopgap refuses instead of
/// emitting Gleam that cannot compile (2026-07-11 emitter panel, blocking
/// finding; the checker refuses the same shape in the pre-loop scope).
#[test]
fn loop_max_must_be_loop_invariant() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Loop ceiling stress.
workflow loop_ceiling
  input job: String
  outcome done: type Report, route success

type Round  { summary: String, gates_green: Bool, budget: Int }
type Report { summary: String }

worker builder
  action work(prior: Round) -> Round

step build
  loop round = Round(summary: \"\", gates_green: false, budget: 3) counting cycles
    work(prior: round) -> round
    until round.gates_green
    max round.budget

  outcome green: when round.gates_green, route done(summary: round.summary)
  outcome spent: otherwise, route done(summary: \"spent\")
";
    let document = parse(source)?;
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("before the loop"),
        "unexpected message: {}",
        error.message
    );
    let max_line = source
        .lines()
        .position(|line| line.contains("max round.budget"))
        .ok_or("missing max line")?
        + 1;
    assert_eq!(error.span.line, max_line, "span must anchor the ceiling");
    Ok(())
}

/// One binding name bound with two different types in disjoint route
/// branches is refused: a name-keyed first-wins map would annotate the
/// other branch's region parameters wrongly and emit non-compiling Gleam
/// (2026-07-11 emitter panel, blocking finding).
#[test]
fn conflicting_binding_types_across_branches_are_refused() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Branch type conflict stress.
workflow branch_conflict
  input essay: String
  outcome done_a: type AOut, route success
  outcome done_b: type BOut, route failure

type Flag { which: Bool }
type A    { left: String }
type B    { right: Int }
type AOut { value: String }
type BOut { value: Int }

worker shape
  action probe(essay: String) -> Flag
  action make_a() -> A
  action make_b() -> B
  action show_a(item: A) -> AOut
  action show_b(item: B) -> BOut

step decide
  probe(essay: essay) -> flag

  outcome go_a: when flag.which, route path_a
  outcome go_b: otherwise, route path_b

step path_a
  make_a() -> item
  route join_a

step path_b
  make_b() -> item
  route join_b

step join_a
  show_a(item: item) -> out
  route done_a(value: out.value)

step join_b
  show_b(item: item) -> outb
  route done_b(value: outb.value)
";
    let document = parse(source)?;
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("one type per binding name"),
        "unexpected message: {}",
        error.message
    );
    let second_bind = source
        .lines()
        .position(|line| line.contains("make_b() -> item"))
        .ok_or("missing conflicting bind")?
        + 1;
    assert_eq!(
        error.span.line, second_bind,
        "span must anchor the conflicting bind"
    );
    Ok(())
}

/// Float ordering comparisons render Gleam's Float operator family
/// (`>.` etc.) — the bare operators are Int-only and fail `gleam build`
/// (2026-07-11 emitter panel, blocking finding).
#[test]
fn float_orderings_render_float_operators() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("loop-outcomes/valid/float_threshold_guard.awl")?;
    assert!(
        generated.contains("case verdict.score >. 0.5 {"),
        "a Float guard must use the Float ordering operator: {generated}"
    );
    Ok(())
}

/// A piped route that also constructs a payload is a checker error; `emit()`
/// on the unchecked document refuses instead of silently dropping the piped
/// value (library callers get the documented `EmitError`).
#[test]
fn piped_route_with_payload_is_refused_at_emit() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Piped payload conflict.
workflow piped_conflict
  input name: String
  outcome done: type Greeting, route success

type Greeting { greeting: String }

worker greeter
  action greet(name: String) -> Greeting

step greet
  name |> greet |> route done(greeting: \"fixed\")
";
    let document = parse(source)?;
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("piped route"),
        "unexpected message: {}",
        error.message
    );
    Ok(())
}

/// A schema import without the document's directory refuses with a spanned
/// error naming the path (defect-discipline: spans stay source-correct).
#[test]
fn schema_import_without_root_is_a_spanned_emit_error() -> Result<(), Box<dyn Error>> {
    let source = fs::read_to_string(rev2_root().join("flagship/valid/dev_brief.awl"))?;
    let document = parse(&source)?;
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("schemas/brief.schema.json"),
        "unexpected message: {}",
        error.message
    );
    let quoted = &source[error.span.start..error.span.end];
    assert!(
        quoted.contains("schemas/brief.schema.json"),
        "span must anchor the import path, got `{quoted}`"
    );
    Ok(())
}

/// Flagship compile proof: `awl_hello` and `dev_brief` build under real
/// `gleam build` against the local SDK (#248: if this fails under parallel
/// load, re-run isolated once before treating it as red).
#[test]
fn flagship_fixtures_compile_under_gleam() -> Result<(), Box<dyn Error>> {
    compile_generated_modules(
        "flagship",
        &[
            (
                "awl_hello",
                emitted_fixture("flagship/valid/awl_hello.awl")?,
            ),
            (
                "dev_brief",
                emitted_fixture("flagship/valid/dev_brief.awl")?,
            ),
        ],
    )
}

/// Loop/fork-heavy compile proof: the combined fixtures exercising loops,
/// counters, forks in all three forms, waits, routes, and compensation all
/// build under real `gleam build`.
#[test]
fn loop_and_fork_fixtures_compile_under_gleam() -> Result<(), Box<dyn Error>> {
    compile_generated_modules(
        "loop_fork",
        &[
            (
                "ship_release",
                emitted_fixture("loop-outcomes/valid/ship_release_combined.awl")?,
            ),
            (
                "release_pipeline",
                emitted_fixture("dag-fork/valid/release_pipeline_combined.awl")?,
            ),
            (
                "fork_named",
                emitted_fixture("dag-fork/valid/fork_named_branches.awl")?,
            ),
            (
                "backward_route",
                emitted_fixture("loop-outcomes/valid/backward_route_bounded_cycle.awl")?,
            ),
            (
                "substeps",
                emitted_fixture("loop-outcomes/valid/substeps_two_stage.awl")?,
            ),
            (
                "combinators",
                emitted_fixture("step-bodies/valid/combinators.awl")?,
            ),
            (
                "float_guard",
                emitted_fixture("loop-outcomes/valid/float_threshold_guard.awl")?,
            ),
            (
                "step_bodies",
                emitted_fixture("step-bodies/valid/step_bodies_combined.awl")?,
            ),
            (
                "mixed_doors",
                emitted_fixture("schema-doors/valid/mixed_doors.awl")?,
            ),
            (
                "declarations",
                emitted_fixture("declarations/valid/declarations_combined.awl")?,
            ),
        ],
    )
}

fn compile_generated_modules(
    project_name: &str,
    modules: &[(&str, String)],
) -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().and_then(Path::parent).ok_or_else(|| {
        io::Error::other("failed to resolve repository root from CARGO_MANIFEST_DIR")
    })?;
    let project = unique_temp_project(project_name);
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("gleam.toml"),
        format!(
            "name = \"awl_{project_name}_compile_proof\"\nversion = \"0.1.0\"\ntarget = \
             \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"{}\" }}\ngleam_stdlib = \
             \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n",
            repo_root.join("gleam/aion_flow").display()
        ),
    )?;
    for (module_name, module_source) in modules {
        fs::write(
            project.join("src").join(format!("{module_name}.gleam")),
            module_source,
        )?;
    }
    let output = Command::new("gleam")
        .arg("build")
        .current_dir(&project)
        .output()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("gleam binary is required for AWL emitter compile-proof tests: {error}"),
            )
        })?;
    assert!(
        output.status.success(),
        "gleam build failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn unique_temp_project(project_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "aion_awl_{project_name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    path
}
