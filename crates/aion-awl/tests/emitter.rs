//! Integration tests for AWL-to-Gleam emission.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl::{emit, parse};

fn emitted(source: &str) -> Result<String, Box<dyn Error>> {
    Ok(emit(&parse(source)?)?)
}

fn assert_golden(source: &str, golden: &str) -> Result<(), Box<dyn Error>> {
    assert_eq!(emitted(source)?, golden);
    Ok(())
}

#[test]
fn emits_research_report_golden() -> Result<(), Box<dyn Error>> {
    assert_golden(
        include_str!("fixtures/research_report.awl"),
        include_str!("fixtures/research_report.gleam.golden"),
    )
}

#[test]
fn emits_hello_golden() -> Result<(), Box<dyn Error>> {
    assert_golden(
        include_str!("fixtures/hello.awl"),
        include_str!("fixtures/hello.gleam.golden"),
    )
}

#[test]
fn emits_bounded_cycle_golden() -> Result<(), Box<dyn Error>> {
    assert_golden(
        include_str!("fixtures/bounded_cycle.awl"),
        include_str!("fixtures/bounded_cycle.gleam.golden"),
    )
}

#[test]
fn emitted_sources_do_not_call_direct_nondeterministic_runtime_apis() -> Result<(), Box<dyn Error>>
{
    let emitted_sources = [
        emitted(include_str!("fixtures/hello.awl"))?,
        emitted(include_str!("fixtures/research_report.awl"))?,
        emitted(include_str!("fixtures/bounded_cycle.awl"))?,
    ];
    let denylist = ["erlang/", "os.", "io.", "random.", "calendar."];
    for source in emitted_sources {
        for denied in denylist {
            assert!(
                !source.contains(denied),
                "generated source contains forbidden direct runtime API {denied}"
            );
        }
    }
    Ok(())
}

/// `on timeout / finish e` must TERMINATE the workflow with `e`: the timed-out
/// arm returns the workflow output directly and none of the subsequent steps
/// run in that arm (they are nested inside the success arm instead).
#[test]
fn on_timeout_finish_terminates_workflow() -> Result<(), Box<dyn Error>> {
    let source = emitted(include_str!("fixtures/research_report.awl"))?;
    let arm = "Error(error.TimedOutError(_)) -> Ok(Published(report: draft, url: \"\"))";
    let arm_index = source
        .find(arm)
        .ok_or("timed-out arm must return the workflow output directly")?;
    // Every subsequent step call site is nested inside the success arm, i.e.
    // BEFORE the timeout arm; nothing step-like appears after it in execute.
    for call in [
        "synthesize_activity(findings, approval.notes) |> workflow.run",
        "upload_assets_activity(draft) |> workflow.run",
        "publish_activity(draft, assets) |> workflow.run",
        "delete_assets_activity(assets) |> workflow.run",
        "Ok(Published(report: draft, url: url))",
    ] {
        let call_index = source.find(call).ok_or("expected call site missing")?;
        assert!(
            call_index < arm_index,
            "step code `{call}` must be nested in the success arm, before the timeout arm"
        );
        assert!(
            !source[arm_index..].contains(call),
            "step code `{call}` must not appear after the timeout arm"
        );
    }
    Ok(())
}

/// `on failure / finish e` must also terminate the workflow: the failure arm
/// runs the compensation `do` lines then returns the workflow output, and the
/// following steps are nested inside the success arm.
#[test]
fn on_failure_finish_terminates_workflow() -> Result<(), Box<dyn Error>> {
    let source = emitted(
        "workflow compensated\n\
         about Failure handler that finishes the workflow.\n\
         \n\
         input token: String\n\
         output Outcome\n\
         \n\
         type Outcome { done: Bool, detail: String }\n\
         \n\
         action risky(token: String) -> Outcome\n\
         action cleanup(token: String) -> Nil\n\
         action extra(token: String) -> Nil\n\
         \n\
         step risky\n\
         \x20 do risky(token)\n\
         \x20 on failure\n\
         \x20   do cleanup(token)\n\
         \x20   finish Outcome(done: false, detail: \"compensated\")\n\
         \x20 as outcome\n\
         \n\
         step extra\n\
         \x20 do extra(token)\n\
         \n\
         finish outcome\n",
    )?;
    let arm_index = source.find("Error(_) -> {").ok_or("failure arm missing")?;
    let arm_close = source[arm_index..]
        .find("Ok(Outcome(done: False, detail: \"compensated\"))")
        .ok_or("failure arm must return the workflow output")?;
    let arm_slice = &source[arm_index..arm_index + arm_close];
    assert!(
        arm_slice.contains("cleanup_activity(token) |> workflow.run"),
        "failure arm must run the compensation do-lines first"
    );
    assert!(
        !arm_slice.contains("extra_activity"),
        "no subsequent step may run inside the failure arm"
    );
    // The subsequent step is nested inside the success arm, before the
    // failure arm; it must not flow after the handler either.
    let extra_index = source
        .find("extra_activity(token) |> workflow.run")
        .ok_or("subsequent step call missing")?;
    assert!(
        extra_index < arm_index,
        "subsequent steps nest in the success arm"
    );
    assert!(!source[arm_index..].contains("extra_activity(token) |> workflow.run"));
    Ok(())
}

/// `repeat up to <e> / until <c>` lowers to a bounded tail-recursive loop
/// threading the rebound `as` value, exiting on the `until` condition or the
/// cap.
#[test]
fn repeat_until_emits_bounded_loop() -> Result<(), Box<dyn Error>> {
    let source = emitted(include_str!("fixtures/bounded_cycle.awl"))?;
    assert!(
        source.contains("fix_loop(3, state)"),
        "execute must invoke the loop with the cap and the threaded binding"
    );
    assert!(source.contains("fn fix_loop(remaining, state) {"));
    assert!(
        source.contains("case remaining <= 0 {"),
        "the loop must stop at the cap"
    );
    assert!(
        source.contains("case state.accepted {"),
        "the loop must check the until condition after each round"
    );
    assert!(
        source.contains("False -> fix_loop(remaining - 1, state)"),
        "the loop must thread the rebound value into the next iteration"
    );
    Ok(())
}

/// A `repeat`/`until` loop whose `as` binding is fresh (no prior binding of
/// that name exists before the step) must not thread the `as` name as one
/// of the loop's free-name call arguments: the loop body binds it itself on
/// every iteration, so treating it as free produces a call site that
/// references a name unbound at the call site.
#[test]
fn fresh_repeat_binding_is_not_a_free_loop_param() -> Result<(), Box<dyn Error>> {
    let source = "workflow fresh_repeat\n\
         input token: String\n\
         output Bool\n\
         type Attempt { done: Bool }\n\
         action work(token: String) -> Attempt\n\
         step w\n\
         \x20 repeat up to 3\n\
         \x20 do work(token)\n\
         \x20 as result\n\
         \x20 until result.done\n\
         finish result.done\n";
    let emitted = emitted(source)?;
    assert!(
        emitted.contains("w_loop(3, token)"),
        "the call site must not thread the fresh `as` binding as an argument: {emitted}"
    );
    assert!(
        !emitted.contains("w_loop(3, token, result)"),
        "`result` must not leak into the loop's free-name parameters: {emitted}"
    );
    compile_generated_module("fresh_repeat", &emitted)
}

/// Child calls lower to the SDK's string-name spawn with a JSON-encoded
/// argument record; no phantom child module is referenced, and step retry
/// config reaches the child call path.
#[test]
fn child_call_lowers_to_string_name_spawn() -> Result<(), Box<dyn Error>> {
    let source = emitted(include_str!("fixtures/bounded_cycle.awl"))?;
    assert!(source.contains("workflow.spawn_and_wait(\"fix_round\""));
    assert!(
        source.contains("json.object([#(\"state\", review_state_to_json(state))])"),
        "named child args must encode as one JSON object"
    );
    assert!(
        source.contains("review_state_codec()"),
        "a rebound child result decodes with the established binding codec"
    );
    assert!(
        source.contains("awl_retry(2, 5000, 2, 60000, fn() {"),
        "step retry config must thread into the child call"
    );
    assert!(!source.contains("fix_round.execute"));
    assert!(!source.contains("fix_round.input_codec"));
    Ok(())
}

/// Step-level queue/node routing and exponential backoff reach the emitted
/// activity configuration.
#[test]
fn queue_node_and_backoff_reach_activity_config() -> Result<(), Box<dyn Error>> {
    let source = emitted(
        "workflow routed\n\
         input token: String\n\
         output String\n\
         action work(token: String) -> String\n\
         step work\n\
         \x20 do work(token)\n\
         \x20 retry 4 backoff 2s..1m\n\
         \x20 queue \"gpu\"\n\
         \x20 node \"sydney\"\n\
         \x20 as result\n\
         finish result\n",
    )?;
    assert!(source.contains(
        "activity.retry(activity.RetryPolicy(max_attempts: 4, backoff: activity.Exponential(initial: duration.milliseconds(2000), multiplier: 2.0, max: duration.milliseconds(60000))))"
    ));
    assert!(source.contains("activity.task_queue(\"gpu\")"));
    assert!(source.contains("activity.node(\"sydney\")"));
    Ok(())
}

/// AWL identifiers that are Gleam reserved words gain a trailing underscore
/// at every emission site, while wire-format JSON keys keep the AWL name.
#[test]
fn gleam_keywords_are_sanitized_everywhere() -> Result<(), Box<dyn Error>> {
    let source = emitted(
        "workflow keywords\n\
         input let: String\n\
         output String\n\
         action greet(fn: String) -> String\n\
         step greet\n\
         \x20 do greet(let)\n\
         \x20 as use\n\
         finish use\n",
    )?;
    assert!(
        source.contains("let let_ = input.let_"),
        "input binding and field access"
    );
    assert!(source.contains("let_: String,"), "input record field");
    assert!(
        source.contains("#(\"let\", string_to_json(keywords_input.let_)),"),
        "JSON key keeps the AWL name"
    );
    assert!(
        source.contains("use let_ <- decode.field(\"let\", string_decoder())"),
        "decoder binding"
    );
    assert!(source.contains("fn_: String,"), "action param field");
    assert!(
        source.contains("greet_activity(let_)"),
        "sanitized reference at the call site"
    );
    assert!(
        source.contains("let assert Ok(use_) ="),
        "sanitized as-binding"
    );
    assert!(source.contains("Ok(use_)"), "sanitized finish reference");
    assert!(
        !source.contains("input.let\n"),
        "no unsanitized field access"
    );
    Ok(())
}

/// `each` on a non-action step is an emit-time error rather than emitted
/// non-compiling or failing code.
#[test]
fn each_on_non_action_step_is_an_emit_error() -> Result<(), Box<dyn Error>> {
    let document = parse(
        "workflow bad_each\n\
         input names: List(String)\n\
         output Bool\n\
         signal go: Bool\n\
         step bad\n\
         \x20 each name in names\n\
         \x20 wait go\n\
         \x20 as flag\n\
         finish flag\n",
    )?;
    let error = emit(&document).err().ok_or("emit must fail")?;
    assert!(
        error
            .message
            .contains("`each` is only supported for action calls"),
        "unexpected message: {}",
        error.message
    );
    Ok(())
}

/// A `when`-guarded rebind of a name with no prior binding is an emit-time
/// error (the false arm would reference an unbound name).
#[test]
fn when_guarded_fresh_binding_is_an_emit_error() -> Result<(), Box<dyn Error>> {
    let document = parse(
        "workflow bad_guard\n\
         input flag: Bool\n\
         output String\n\
         action make() -> String\n\
         step guarded\n\
         \x20 when flag\n\
         \x20 do make()\n\
         \x20 as fresh\n\
         finish fresh\n",
    )?;
    let error = emit(&document).err().ok_or("emit must fail")?;
    assert!(
        error.message.contains("no prior binding"),
        "unexpected message: {}",
        error.message
    );
    Ok(())
}

/// Queue/node routing on a child call cannot be honored by the SDK spawn API
/// and must be an emit-time error instead of being silently dropped.
#[test]
fn queue_on_child_call_is_an_emit_error() -> Result<(), Box<dyn Error>> {
    let document = parse(
        "workflow routed_child\n\
         input token: String\n\
         output String\n\
         step call\n\
         \x20 do child worker(token)\n\
         \x20 queue \"gpu\"\n\
         \x20 as token\n\
         finish token\n",
    )?;
    let error = emit(&document).err().ok_or("emit must fail")?;
    assert!(
        error
            .message
            .contains("not supported on child workflow calls"),
        "unexpected message: {}",
        error.message
    );
    Ok(())
}

/// A child-workflow result bound fresh (`as fresh_result` with no prior
/// binding of that name) is opaque: the checker has no contract for it in
/// this revision. Referencing it in a later expression — here, as an
/// activity-call argument — must be a spanned emit-time error rather than
/// emitted Gleam that fails to typecheck (the opaque value decodes as
/// `Nil`, mismatching whatever type the site expects).
#[test]
fn opaque_child_result_in_expression_is_an_emit_error() -> Result<(), Box<dyn Error>> {
    let source = "workflow opaque_leak\n\
         input token: String\n\
         output String\n\
         action consume(value: String) -> String\n\
         step spawn\n\
         \x20 do child worker(token)\n\
         \x20 as fresh_result\n\
         step consume\n\
         \x20 do consume(fresh_result)\n\
         \x20 as result\n\
         finish result\n";
    let document = parse(source)?;
    let error = emit(&document).err().ok_or("emit must fail")?;
    assert!(
        error.message.contains("fresh_result")
            && error.message.contains("untyped in this revision")
            && error.message.contains("cannot be used in expressions"),
        "unexpected message: {}",
        error.message
    );
    let call_site = source
        .find("do consume(fresh_result)")
        .ok_or("expected call site missing")?;
    let expected_start = call_site + "do consume(".len();
    assert_eq!(
        error.span.start, expected_start,
        "span must point at the offending reference in the later expression, \
         not the call keyword or the earlier `as fresh_result` binding site"
    );
    assert_eq!(&source[error.span.start..error.span.end], "fresh_result");
    Ok(())
}

#[test]
fn emitted_hello_compiles_against_local_aion_flow() -> Result<(), Box<dyn Error>> {
    compile_generated_module("hello", &emitted(include_str!("fixtures/hello.awl"))?)
}

#[test]
fn emitted_research_report_compiles_against_local_aion_flow() -> Result<(), Box<dyn Error>> {
    compile_generated_module(
        "research_report",
        &emitted(include_str!("fixtures/research_report.awl"))?,
    )
}

#[test]
fn emitted_bounded_cycle_compiles_against_local_aion_flow() -> Result<(), Box<dyn Error>> {
    compile_generated_module(
        "bounded_cycle",
        &emitted(include_str!("fixtures/bounded_cycle.awl"))?,
    )
}

fn compile_generated_module(module_name: &str, module_source: &str) -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().and_then(Path::parent).ok_or_else(|| {
        io::Error::other("failed to resolve repository root from CARGO_MANIFEST_DIR")
    })?;
    let project = unique_temp_project(module_name);
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("gleam.toml"),
        format!(
            "name = \"awl_{module_name}_compile_proof\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"{}\" }}\ngleam_stdlib = \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n",
            repo_root.join("gleam/aion_flow").display()
        ),
    )?;
    fs::write(
        project.join("src").join(format!("{module_name}.gleam")),
        module_source,
    )?;

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

fn unique_temp_project(module_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "aion_awl_{module_name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    path
}
