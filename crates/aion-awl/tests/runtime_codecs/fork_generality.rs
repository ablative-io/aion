//! Execution-parity proofs for the fork-generality lane's config surface:
//! call-site config on plain calls and fork branches (the per-key
//! site-over-declaration merge) and forks inside loops. The statement-level
//! ride-alongs (child statements, waits, index/list expressions) live in
//! `stmt_parity`, sharing this module's helpers. Direct-selected modules and
//! reference-emitted (gleam-built) modules run the SAME driver values in one
//! embedded beamr VM; results must match byte for byte.
//!
//! Echo strategy: `collect_echo_ebin`/`dispatch_echo_ebin` record the full
//! wire (per-item inputs, merged per-branch config JSON, dispatch order) in
//! the workflow process dictionary before refusing dispatch, and a
//! per-test `aion_awl_echo_runner` reads it back in the same process —
//! `{Result, Echo}`. The AWL error mapper deliberately collapses activity
//! errors (`awl/error.gleam::map_activity_error`), so the workflow result
//! alone could never distinguish config bytes; the echo can. Byte parity of
//! the `{Result, Echo}` pair proves the entire dispatch surface without an
//! engine.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl::mir::{Block, FnRef, MirModule, Tail, Value, lower, select};
use aion_awl::parse;

use super::drivers::{Body, atom_ref, lit_ref, push_driver};
use super::harness::{
    build_vm, collect_echo_ebin, dispatch_echo_ebin, gleam_build, manifest_dir,
    reference_module_at, scratch_build_dir, wait_ffi_ebin,
};

type TestResult = Result<(), Box<dyn Error>>;

pub(crate) fn parity_fixture(name: &str) -> PathBuf {
    manifest_dir().join("tests/fixtures/parity").join(name)
}

/// Lower a parity document (they live outside the rev2 fixture tree, so the
/// BC-3 oracle and the COVERED ratchet keep their fixed denominator).
pub(crate) fn lowered_at(path: &Path) -> Result<MirModule, Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    let document = parse(&source)?;
    Ok(lower(&document, path.parent())?)
}

/// Append `awl$rt_execute` calling the production `execute/1` host on the
/// prepared input value.
pub(crate) fn push_execute(module: &mut MirModule, name: &str, mut body: Body, input_value: Value) {
    let result = body.call_local(FnRef(2), vec![input_value]);
    push_driver(
        module,
        name,
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(result)),
        },
    );
}

/// Build the per-test echo runner: one erlc module dispatching to the direct
/// and reference `awl$rt_execute`/`awl_rt_execute` drivers by side atom in a
/// workflow-sized process, and returning `{Result, Echo}` where `Echo` is
/// the wire recording the ffi echo stub left in that process's dictionary.
pub(crate) fn echo_runner_ebin(
    label: &str,
    direct_module: &str,
    ref_module: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let dir = scratch_build_dir(&format!("echo_runner_{label}"));
    fs::create_dir_all(&dir)?;
    let runner = dir.join("aion_awl_echo_runner.erl");
    fs::write(
        &runner,
        format!(
            "-module(aion_awl_echo_runner).\n\
             -export([run/1, direct_target/1, ref_target/1]).\n\
             run(direct) -> run_target(direct_target);\n\
             run(ref) -> run_target(ref_target).\n\
             run_target(Target) ->\n\
             Parent = self(),\n\
             _ = erlang:spawn_opt(?MODULE, Target, [Parent], [{{min_heap_size, 4096}}]),\n\
             receive {{aion_awl_echo_result, Result}} -> Result end.\n\
             direct_target(Parent) ->\n\
             Result = {direct_module}:'awl$rt_execute'(),\n\
             Parent ! {{aion_awl_echo_result, {{Result, echo()}}}}.\n\
             ref_target(Parent) ->\n\
             Result = {ref_module}:awl_rt_execute(),\n\
             Parent ! {{aion_awl_echo_result, {{Result, echo()}}}}.\n\
             echo() ->\n\
             case erlang:get(awl_ffi_echo) of\n\
             undefined -> <<\"no echo\">>; Echo -> Echo end.\n"
        ),
    )?;
    let output = Command::new("erlc")
        .arg("-o")
        .arg(&dir)
        .arg(&runner)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "echo runner erlc failed for {label}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(dir)
}

// ---- ride-along 1: plain call-site config merges per key -------------------

const REF_CALL_CONFIG_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(CallConfigInput(url: "https://example.test/report"))
}
"#;

#[test]
fn plain_call_site_config_merges_per_key_with_reference_parity() -> TestResult {
    let path = parity_fixture("call_config.awl");
    let reference = reference_module_at(&path, REF_CALL_CONFIG_DRIVER)?;
    let mut ebins = gleam_build(&[("ref_call_config", &reference)])?;
    ebins.push(dispatch_echo_ebin()?);

    ebins.push(echo_runner_ebin(
        "call_config",
        "call_config",
        "ref_call_config",
    )?);

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "call_config_input");
    let url = lit_ref(&mut direct, "https://example.test/report");
    let mut body = Body::new();
    let input_value = body.record(input, vec![Value::Lit(url)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));
    let direct_bytes = select(&direct)?;

    let vm = build_vm(&ebins, &[direct_bytes])?;
    let direct_result = vm.call_echo("direct")?;
    let reference_result = vm.call_echo("ref")?;
    assert_eq!(direct_result, reference_result, "plain call config parity");
    assert!(
        direct_result.contains("edge01"),
        "the site node override must reach the wire: {direct_result}"
    );
    assert!(
        !direct_result.contains("decl_host"),
        "the declaration node must be overridden per key: {direct_result}"
    );
    assert!(
        direct_result.contains("max_attempts"),
        "the DECLARATION retry must survive the merge (site sets none): {direct_result}"
    );
    assert!(
        direct_result.contains("1200000"),
        "the declaration timeout (20m) must survive the merge: {direct_result}"
    );
    Ok(())
}

// ---- fork-branch call-site config (BC-2b-5 closed) --------------------------

const REF_FORK_CONFIG_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(ForkConfigInput(halves: [Half(name: "a"), Half(name: "b")]))
}
"#;

#[test]
fn parallel_action_fork_site_config_dispatches_with_reference_parity() -> TestResult {
    let path = parity_fixture("fork_config.awl");
    let reference = reference_module_at(&path, REF_FORK_CONFIG_DRIVER)?;
    let mut ebins = gleam_build(&[("ref_fork_config", &reference)])?;
    ebins.push(collect_echo_ebin()?);

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "fork_config_input");
    let half = atom_ref(&mut direct, "half");
    let a = lit_ref(&mut direct, "a");
    let b = lit_ref(&mut direct, "b");
    let mut body = Body::new();
    let item_a = body.record(half, vec![Value::Lit(a)]);
    let item_b = body.record(half, vec![Value::Lit(b)]);
    let halves = body.list(vec![Value::Var(item_a), Value::Var(item_b)]);
    let input_value = body.record(input, vec![Value::Var(halves)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));
    let direct_bytes = select(&direct)?;

    ebins.push(echo_runner_ebin(
        "fork_config",
        "fork_config",
        "ref_fork_config",
    )?);
    let vm = build_vm(&ebins, &[direct_bytes])?;
    let direct_result = vm.call_echo("direct")?;
    let reference_result = vm.call_echo("ref")?;
    assert_eq!(
        direct_result, reference_result,
        "parallel fork config parity"
    );
    assert!(
        direct_result.contains("edge01"),
        "the branch node override must reach every spec: {direct_result}"
    );
    assert!(
        direct_result.contains("300000"),
        "the branch timeout override (5m) must reach the wire: {direct_result}"
    );
    assert!(
        !direct_result.contains("decl_host") && !direct_result.contains("1200000"),
        "overridden declaration keys must not leak: {direct_result}"
    );
    Ok(())
}

const REF_FORK_CONFIG_SEQ_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(ForkConfigSeqInput(halves: [Half(name: "lead")], revisions: ["r1", "r2"]))
}
"#;

#[test]
fn sequential_fork_site_config_and_index_prelude_dispatch_with_reference_parity() -> TestResult {
    let path = parity_fixture("fork_config_seq.awl");
    let reference = reference_module_at(&path, REF_FORK_CONFIG_SEQ_DRIVER)?;
    let mut ebins = gleam_build(&[("ref_fork_config_seq", &reference)])?;
    ebins.push(dispatch_echo_ebin()?);

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "fork_config_seq_input");
    let half = atom_ref(&mut direct, "half");
    let lead = lit_ref(&mut direct, "lead");
    let r1 = lit_ref(&mut direct, "r1");
    let r2 = lit_ref(&mut direct, "r2");
    let mut body = Body::new();
    let item = body.record(half, vec![Value::Lit(lead)]);
    let halves = body.list(vec![Value::Var(item)]);
    let revisions = body.list(vec![Value::Lit(r1), Value::Lit(r2)]);
    let input_value = body.record(input, vec![Value::Var(halves), Value::Var(revisions)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));
    let direct_bytes = select(&direct)?;

    ebins.push(echo_runner_ebin(
        "fork_config_seq",
        "fork_config_seq",
        "ref_fork_config_seq",
    )?);
    let vm = build_vm(&ebins, &[direct_bytes])?;
    let direct_result = vm.call_echo("direct")?;
    let reference_result = vm.call_echo("ref")?;
    assert_eq!(
        direct_result, reference_result,
        "sequential fork config parity"
    );
    // The first item's dispatch echo: the branch's index prelude (halves[0])
    // and the first revision must reach the per-item wire, the site timeout
    // (5m) overrides, and the declaration node survives.
    assert!(
        direct_result.contains("lead") && direct_result.contains("r1"),
        "the sequential branch input (index prelude + item) drifted: {direct_result}"
    );
    assert!(
        direct_result.contains("300000") && direct_result.contains("decl_host"),
        "the per-key merge drifted (site timeout + declaration node): {direct_result}"
    );
    Ok(())
}

const REF_FORK_NAMED_CONFIG_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(ForkNamedConfigInput(primary: "east", secondary: "west"))
}
"#;

#[test]
fn named_fork_homogeneous_site_config_parity() -> TestResult {
    let path = parity_fixture("fork_named_config.awl");
    let reference = reference_module_at(&path, REF_FORK_NAMED_CONFIG_DRIVER)?;
    let mut ebins = gleam_build(&[("ref_fork_named_config", &reference)])?;
    ebins.push(collect_echo_ebin()?);

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "fork_named_config_input");
    let east = lit_ref(&mut direct, "east");
    let west = lit_ref(&mut direct, "west");
    let mut body = Body::new();
    let input_value = body.record(input, vec![Value::Lit(east), Value::Lit(west)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));
    let direct_bytes = select(&direct)?;

    ebins.push(echo_runner_ebin(
        "fork_named_config",
        "fork_named_config",
        "ref_fork_named_config",
    )?);
    let vm = build_vm(&ebins, &[direct_bytes])?;
    let direct_result = vm.call_echo("direct")?;
    let reference_result = vm.call_echo("ref")?;
    assert_eq!(direct_result, reference_result, "named homogeneous parity");
    // Both branch configs present, in source order (east01 before west01).
    let east_at = direct_result
        .find("east01")
        .ok_or("first branch node pin missing")?;
    let west_at = direct_result
        .find("west01")
        .ok_or("second branch node pin missing")?;
    assert!(
        east_at < west_at,
        "branch configs must ride in source order: {direct_result}"
    );
    Ok(())
}

const REF_FORK_NAMED_HETERO_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(ForkNamedHeteroConfigInput(user_id: "u1"))
}
"#;

#[test]
fn named_fork_heterogeneous_site_config_parity() -> TestResult {
    let path = parity_fixture("fork_named_hetero_config.awl");
    let reference = reference_module_at(&path, REF_FORK_NAMED_HETERO_DRIVER)?;
    let mut ebins = gleam_build(&[("ref_fork_named_hetero_config", &reference)])?;
    ebins.push(collect_echo_ebin()?);

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "fork_named_hetero_config_input");
    let user = lit_ref(&mut direct, "u1");
    let mut body = Body::new();
    let input_value = body.record(input, vec![Value::Lit(user)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));
    let direct_bytes = select(&direct)?;

    ebins.push(echo_runner_ebin(
        "fork_named_hetero_config",
        "fork_named_hetero_config",
        "ref_fork_named_hetero_config",
    )?);
    let vm = build_vm(&ebins, &[direct_bytes])?;
    let direct_result = vm.call_echo("direct")?;
    let reference_result = vm.call_echo("ref")?;
    assert_eq!(
        direct_result, reference_result,
        "named heterogeneous parity"
    );
    // The raw-twin path still carries the configured branch's node pin and
    // both wire-identical raw inputs.
    assert!(
        direct_result.contains("archive01"),
        "the configured hetero branch lost its node pin: {direct_result}"
    );
    assert!(
        direct_result.contains("fetch_profile") && direct_result.contains("fetch_history"),
        "both hetero branches must dispatch in one collect: {direct_result}"
    );
    assert!(
        direct_result.matches("u1").count() >= 2,
        "both raw inputs must carry the encoded argument: {direct_result}"
    );
    Ok(())
}

// ---- fork inside a loop: two passes, completing fan-outs ---------------------

const REF_FORK_IN_LOOP_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(ForkInLoopInput(items: ["seed"], max_rounds: 2))
}
"#;

/// The completing collector: `prepare` yields a two-item batch, every
/// per-pass `workflow.map` collect completes with two DISTINCT verdict
/// payloads (order-observable), and `fold` echoes its encoded input —
/// which embeds the joined verdicts in join order — into the process
/// dictionary, one entry per loop pass.
const FORK_IN_LOOP_FFI: &str = r#"-module(aion_flow_ffi).
-export([dispatch_activity/3, collect_all/2]).
dispatch_activity(<<"prepare">>, _Input, _Config) ->
    {ok, <<"{\"items\":[\"a\",\"b\"]}">>};
dispatch_activity(<<"fold">>, Input, _Config) ->
    Prior = case erlang:get(awl_ffi_echo) of undefined -> <<>>; P -> P end,
    erlang:put(awl_ffi_echo, <<Prior/binary, "fold:", Input/binary, ";">>),
    {ok, <<"{\"complete\":false}">>}.
collect_all(_Id, _Specs) ->
    {ok, <<"[\"{\\\"blocking\\\":true}\",\"{\\\"blocking\\\":false}\"]">>}.
"#;

#[test]
fn fork_inside_loop_completes_two_passes_with_reference_parity() -> TestResult {
    let path = parity_fixture("fork_in_loop.awl");
    let reference = reference_module_at(&path, REF_FORK_IN_LOOP_DRIVER)?;
    let mut ebins = gleam_build(&[("ref_fork_in_loop", &reference)])?;
    ebins.push(wait_ffi_ebin("fork_in_loop", FORK_IN_LOOP_FFI)?);
    ebins.push(echo_runner_ebin(
        "fork_in_loop",
        "fork_in_loop",
        "ref_fork_in_loop",
    )?);

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "fork_in_loop_input");
    let seed = lit_ref(&mut direct, "seed");
    let mut body = Body::new();
    let items = body.list(vec![Value::Lit(seed)]);
    let input_value = body.record(input, vec![Value::Var(items), Value::Int(2)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));
    let direct_bytes = select(&direct)?;

    let vm = build_vm(&ebins, &[direct_bytes])?;
    let direct_result = vm.call_echo("direct")?;
    let reference_result = vm.call_echo("ref")?;
    assert_eq!(direct_result, reference_result, "fork-in-loop parity");
    // Two passes: two fold echoes, each embedding the joined verdicts in
    // JOIN ORDER (blocking:true before blocking:false — the collect's
    // payload order survives the typed decode and the fold's re-encode).
    assert_eq!(
        direct_result.matches("fold:").count(),
        2,
        "the loop must run exactly two passes: {direct_result}"
    );
    // Scope the order check to the verdicts list (the fold input also
    // carries `prior.complete: false` BEFORE the verdicts).
    let verdicts_at = direct_result
        .find("verdicts")
        .ok_or("fold input lost the verdicts list")?;
    let verdicts = &direct_result[verdicts_at..];
    let first_true = verdicts
        .find("true")
        .ok_or("first verdict payload missing")?;
    let first_false = verdicts
        .find("false")
        .ok_or("second verdict payload missing")?;
    assert!(
        first_true < first_false,
        "joined verdict order drifted from input order: {direct_result}"
    );
    Ok(())
}
