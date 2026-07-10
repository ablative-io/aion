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
