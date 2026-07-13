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
use std::path::{Path, PathBuf};

use aion_awl::{check_in, emit, emit_in, parse};

#[path = "emitter/children.rs"]
mod child_tests;
#[path = "emitter/compile.rs"]
mod compile_tests;
#[path = "emitter/forks.rs"]
mod fork_tests;
#[path = "emitter/loops.rs"]
mod loop_tests;

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

fn assert_fragments_in_order(text: &str, fragments: &[&str]) -> Result<(), Box<dyn Error>> {
    let mut cursor = 0;
    for fragment in fragments {
        let relative = text[cursor..]
            .find(fragment)
            .ok_or_else(|| format!("missing ordered fragment `{fragment}` in:\n{text}"))?;
        cursor += relative + fragment.len();
    }
    Ok(())
}

fn function_after<'a>(generated: &'a str, signature: &str) -> Result<&'a str, Box<dyn Error>> {
    generated
        .split(signature)
        .nth(1)
        .ok_or_else(|| format!("missing generated function `{signature}`").into())
}

/// Pin the reference emitter's loop semantics as the other half of the MIR
/// parity contract, including call-site initialization and control nesting.
#[test]
fn loop_counter_is_language_owned() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("loop-outcomes/valid/loop_counting_until_max.awl")?;
    assert!(
        generated.contains("let awl_count = awl_count + 1"),
        "the generated loop must increment its own counter: {generated}"
    );
    assert!(
        generated.contains("use #(status, polls) <- result.try(poll_loop_0("),
        "the loop must return the threaded value and the counter: {generated}"
    );
    assert!(
        generated.contains(", 0, max_polls, job_id))"),
        "the call site must initialize the hidden count to zero: {generated}"
    );

    let flow = function_after(&generated, "fn poll_loop_0(")?;
    assert_fragments_in_order(
        flow,
        &[
            "poll_activity(job_id, status)",
            "let awl_count = awl_count + 1",
            "case status.done {",
            "True -> Ok(#(status, awl_count))",
            "False ->",
            "case awl_count >= awl_max {",
            "True -> Ok(#(status, awl_count))",
            "False -> poll_loop_0(status, awl_count, awl_max, job_id)",
        ],
    )?;
    assert!(
        generated.contains("poll_activity(job_id, status)"),
        "the worker dispatch must carry only the declared params, never the counter: {generated}"
    );
    // The counter reaches the outcome payloads from the loop's return alone.
    assert!(generated.contains("Finished(job_id: job_id, polls: polls)"));
    Ok(())
}

fn emitted_archived_exam() -> Result<String, Box<dyn Error>> {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .ok_or("failed to resolve repository root")?;
    let path = repo_root.join("docs/design/aion-authoring/awl/exam/workbench/awl_exam.awl");
    let source = fs::read_to_string(&path)?;
    let document = parse(&source)?;
    Ok(emit_in(
        &document,
        path.parent().ok_or("archived exam path has no parent")?,
    )?)
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
        generated.contains("use let_ <- decode.field(\"let\", awlc.string_decoder())"),
        "decoder binding keeps the AWL wire name"
    );
    assert!(generated.contains("fn_: String,"), "action param field");
    assert!(
        generated.contains("use use_ <- result.try(greet_activity(let_)"),
        "sanitized binding and reference at the call site: {generated}"
    );
    assert!(
        generated.contains("#(\"use\", awlc.string_to_json(inner))")
            || generated.contains("#(\"use\", awlc.string_to_json(value.use_))"),
        "JSON key keeps the AWL name: {generated}"
    );
    Ok(())
}

/// Nested boolean expressions use Gleam block grouping. Parentheses are not
/// general-purpose grouping syntax in Gleam and make emitted modules fail to
/// parse (the cargo-gates named-fork verdict exposed this).
#[test]
fn nested_boolean_expressions_use_gleam_block_grouping() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Grouped boolean expression regression.
workflow grouped
  input first: Bool
  input second: Bool
  input third: Bool
  outcome done: type Done, route success

type Done { accepted: Bool }

step decide
  outcome yes: when first and second and third,
    route done(accepted: true)
  outcome no: otherwise,
    route done(accepted: false)
";
    let generated = emitted(source)?;
    assert!(generated.contains("case { first && second } && third {"));
    assert!(!generated.contains("case (first && second) && third {"));
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
        after.contains("Error(awl_error.AwlOutcomeFailure(\"failed\""),
        "compensation must end in its declared failure route: {generated}"
    );
    let success_outcome = generated
        .find(
            "Error(awl_error.AwlOutcomeFailure(\"failed\", json.to_string(publish_failed_to_json(\
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
        parallel.contains("use verdicts <- result.try(workflow.map(docs, fn(doc) {"),
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
        generated.contains("use profile <- result.try(awlc.decoded(profile_codec(), awl_raw_0,"),
        "branch 1 must decode with its return codec: {generated}"
    );
    assert!(
        generated.contains("use history <- result.try(awlc.decoded(history_codec(), awl_raw_1,"),
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
        generated.contains(
            "use scout_report <- result.try(awlc.decoded(scout_report_codec(), awl_raw_0,"
        ),
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
