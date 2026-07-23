//! Execution-parity proofs for the statement-level ride-alongs of the
//! fork-generality lane: indexing over arbitrary bases + non-empty list
//! literals, the awaited child call statement + fire-and-forget `spawn`,
//! and `wait` with and without a timeout (all four case arms). Shares the
//! parity-fixture and echo-runner helpers with `fork_generality`.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use aion_awl::mir::{Block, FnRef, Tail, Value, select};

use super::drivers::{Body, atom_ref, lit_ref, push_driver};
use super::fork_generality::{lowered_at, parity_fixture, push_execute};
use super::harness::{
    build_vm, gleam_build, reference_module_at, scratch_build_dir, wait_ffi_ebin,
};

type TestResult = Result<(), Box<dyn Error>>;

/// One timeout-case arm's semantic expectation on the shared result string.
type ArmCheck = fn(&str) -> bool;

// ---- ride-alongs 4+5: index over a field base + non-empty list literals ----

const REF_INDEX_LISTS_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(IndexListsInput(roster: Roster(reviewers: ["ada", "grace"])))
}

pub fn awl_rt_execute_empty() {
  execute(IndexListsInput(roster: Roster(reviewers: [])))
}
"#;

#[test]
fn index_and_lists_execute_with_reference_parity() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let path = parity_fixture("index_lists.awl");
    let reference = reference_module_at(&path, REF_INDEX_LISTS_DRIVER)?;
    let ebins = gleam_build(&[("ref_index_lists", &reference)])?;

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "index_lists_input");
    let roster = atom_ref(&mut direct, "roster");
    let ada = lit_ref(&mut direct, "ada");
    let grace = lit_ref(&mut direct, "grace");

    let mut body = Body::new();
    let reviewers = body.list(vec![Value::Lit(ada), Value::Lit(grace)]);
    let roster_value = body.record(roster, vec![Value::Var(reviewers)]);
    let input_value = body.record(input, vec![Value::Var(roster_value)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));

    let mut body = Body::new();
    let reviewers = body.list(Vec::new());
    let roster_value = body.record(roster, vec![Value::Var(reviewers)]);
    let input_value = body.record(input, vec![Value::Var(roster_value)]);
    push_execute(
        &mut direct,
        "awl$rt_execute_empty",
        body,
        Value::Var(input_value),
    );

    let direct_bytes = select(&direct)?;
    let vm = build_vm(&ebins, &[direct_bytes])?;

    let direct_full = vm.call0("index_lists", "awl$rt_execute")?;
    let reference_full = vm.call0("ref_index_lists", "awl_rt_execute")?;
    assert_eq!(direct_full, reference_full, "populated execute parity");
    assert!(
        direct_full.starts_with("{ok,"),
        "populated execute did not complete: {direct_full}"
    );
    assert!(
        direct_full.contains("ada!"),
        "index-over-field + concat lost the lead reviewer: {direct_full}"
    );
    assert!(
        direct_full.contains("kickoff") && direct_full.contains("urgent"),
        "the non-empty list literal did not materialize: {direct_full}"
    );

    // Out of range: the byte-identical line/column-anchored runtime message.
    let direct_empty = vm.call0("index_lists", "awl$rt_execute_empty")?;
    let reference_empty = vm.call0("ref_index_lists", "awl_rt_execute_empty")?;
    assert_eq!(direct_empty, reference_empty, "out-of-range parity");
    assert!(
        direct_empty.starts_with("{error,"),
        "out-of-range index must fail the run: {direct_empty}"
    );
    assert!(
        direct_empty.contains("index 0 out of range at line"),
        "the anchored runtime message is missing: {direct_empty}"
    );
    Ok(())
}

// ---- ride-along 2: awaited child call + fire-and-forget spawn ---------------

const ESSAY_JSON: &str = r#"{"essay":"hello"}"#;

const REF_CHILD_CALL_DRIVER: &str = r#"
pub fn awl_rt_run() {
  run(dynamic.string("{\"essay\":\"hello\"}"))
}
"#;

/// The production FFI namespace for the child-statement proof: `spawn_child`
/// executes the generated child's exported `run/1` on the exact encoded
/// input and records its `ok:`-prefixed encoded output under a
/// process-dictionary correlation (the `harness::child_host_ebin`
/// precedent); `await_child` replays it. The fire-and-forget spawn and the
/// awaited call share the child, proving both statement forms cross the same
/// six-argument spawn ABI.
fn child_stmt_host_ebin(
    label: &str,
    parent_module: &str,
    run_fn: &str,
    child_module: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let dir = scratch_build_dir(&format!("child_stmt_host_{label}"));
    fs::create_dir_all(&dir)?;
    let ffi = dir.join("aion_flow_ffi.erl");
    fs::write(
        &ffi,
        format!(
            "-module(aion_flow_ffi).\n-export([spawn_child/3, await_child/1]).\n\
             spawn_child(<<\"score_essay\">>, Input, _Config) ->\n\
             ChildId = Input,\n\
             Result = case {child_module}:run(Input) of\n\
             {{ok, Output}} -> {{ok, <<\"ok:\", Output/binary>>}};\n\
             {{error, _}} -> {{error, <<\"child run failed\">>}} end,\n\
             erlang:put({{awl_child_result, ChildId}}, Result),\n\
             {{ok, ChildId}}.\n\
             await_child(ChildId) ->\n\
             case erlang:get({{awl_child_result, ChildId}}) of\n\
             undefined -> {{error, <<\"unknown child\">>}}; Result -> Result end.\n"
        ),
    )?;
    let runner = dir.join("aion_awl_test_heap.erl");
    fs::write(
        &runner,
        format!(
            "-module(aion_awl_test_heap).\n-export([run/2, target/2]).\n\
             run(_Module, _Function) ->\n\
             Parent = self(),\n\
             _ = erlang:spawn_opt(?MODULE, target, [Parent, run_target], \
             [{{min_heap_size, 2048}}]),\n\
             receive {{aion_awl_test_result, Result}} -> Result end.\n\
             target(Parent, run_target) ->\n\
             Parent ! {{aion_awl_test_result, {parent_module}:{run_fn}()}}.\n"
        ),
    )?;
    let output = Command::new("erlc")
        .arg("-o")
        .arg(&dir)
        .arg(&ffi)
        .arg(&runner)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "child stmt host erlc failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(dir)
}

#[test]
fn child_call_statement_and_spawn_execute_with_reference_parity() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let parent_path = parity_fixture("child_call.awl");
    let child_path = parity_fixture("score_essay.awl");
    let ref_parent = reference_module_at(&parent_path, REF_CHILD_CALL_DRIVER)?;
    let ref_child = reference_module_at(&child_path, "")?;
    let ebins = gleam_build(&[
        ("ref_child_call", &ref_parent),
        ("ref_score_essay_stmt", &ref_child),
    ])?;

    let mut direct_parent = lowered_at(&parent_path)?;
    let input = lit_ref(&mut direct_parent, ESSAY_JSON);
    let mut body = Body::new();
    let result = body.call_local(FnRef(0), vec![Value::Lit(input)]);
    push_driver(
        &mut direct_parent,
        "awl$rt_run",
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(result)),
        },
    );
    let direct_child = lowered_at(&child_path)?;
    let direct_modules = [select(&direct_parent)?, select(&direct_child)?];

    let mut direct_ebins = ebins.clone();
    direct_ebins.push(child_stmt_host_ebin(
        "direct",
        "child_call",
        "'awl$rt_run'",
        "score_essay",
    )?);
    let direct_vm = build_vm(&direct_ebins, &direct_modules)?;
    let direct = direct_vm.call0_large("child_call", "awl$rt_run")?;

    let mut reference_ebins = ebins;
    reference_ebins.push(child_stmt_host_ebin(
        "reference",
        "ref_child_call",
        "awl_rt_run",
        "ref_score_essay_stmt",
    )?);
    let reference_vm = build_vm(&reference_ebins, &[])?;
    let reference = reference_vm.call0_large("ref_child_call", "awl_rt_run")?;

    assert_eq!(direct, reference, "child statement parity");
    assert!(
        direct.starts_with("{ok,"),
        "parent did not complete: {direct}"
    );
    assert!(
        direct.contains("42") && direct.contains("scored: hello"),
        "the awaited child result did not bind into the route payload: {direct}"
    );
    Ok(())
}

// ---- ride-along 3: waits, with and without timeout ---------------------------

const REF_WAIT_SIGNAL_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(WaitSignalInput(change_id: "c1"))
}
"#;

#[test]
fn wait_signal_binds_payload_with_reference_parity() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let path = parity_fixture("wait_signal.awl");
    let reference = reference_module_at(&path, REF_WAIT_SIGNAL_DRIVER)?;
    let mut ebins = gleam_build(&[("ref_wait_signal", &reference)])?;
    ebins.push(wait_ffi_ebin(
        "signal_ok",
        "-module(aion_flow_ffi).\n-export([receive_signal/2]).\n\
         receive_signal(<<\"ruling\">>, _Config) ->\n\
         {ok, <<\"{\\\"note\\\":\\\"approved\\\"}\">>}.\n",
    )?);

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "wait_signal_input");
    let change = lit_ref(&mut direct, "c1");
    let mut body = Body::new();
    let input_value = body.record(input, vec![Value::Lit(change)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));
    let direct_bytes = select(&direct)?;

    let vm = build_vm(&ebins, &[direct_bytes])?;
    let direct_result = vm.call0("wait_signal", "awl$rt_execute")?;
    let reference_result = vm.call0("ref_wait_signal", "awl_rt_execute")?;
    assert_eq!(direct_result, reference_result, "wait signal parity");
    assert!(
        direct_result.starts_with("{ok,") && direct_result.contains("approved"),
        "the decoded signal payload did not bind: {direct_result}"
    );
    Ok(())
}

const REF_WAIT_TIMEOUT_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(WaitTimeoutInput(change_id: "c1"))
}
"#;

#[test]
fn wait_timeout_arms_execute_with_reference_parity() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let path = parity_fixture("wait_timeout.awl");
    let reference = reference_module_at(&path, REF_WAIT_TIMEOUT_DRIVER)?;
    let base_ebins = gleam_build(&[("ref_wait_timeout", &reference)])?;

    let mut direct = lowered_at(&path)?;
    let input = atom_ref(&mut direct, "wait_timeout_input");
    let change = lit_ref(&mut direct, "c1");
    let mut body = Body::new();
    let input_value = body.record(input, vec![Value::Lit(change)]);
    push_execute(&mut direct, "awl$rt_execute", body, Value::Var(input_value));
    let direct_bytes = select(&direct)?;

    // (label, ffi body, expectation on the shared result)
    let arms: &[(&str, &str, ArmCheck)] = &[
        (
            "completes",
            "-module(aion_flow_ffi).\n\
             -export([receive_signal/2, with_timeout/2]).\n\
             receive_signal(<<\"signoff\">>, _Config) ->\n\
             {ok, <<\"{\\\"reviewer\\\":\\\"ada\\\"}\">>}.\n\
             with_timeout(_Deadline, Operation) -> {ok, Operation()}.\n",
            |result| result.starts_with("{ok,") && result.contains("ada"),
        ),
        (
            "timed_out",
            "-module(aion_flow_ffi).\n-export([with_timeout/2]).\n\
             with_timeout(_Deadline, _Operation) -> {error, <<\"timeout:lapsed\">>}.\n",
            // The lapsed arm binds None and takes the FAILURE outcome route.
            |result| result.starts_with("{error,") && result.contains("lapsed"),
        ),
        (
            "inner",
            "-module(aion_flow_ffi).\n\
             -export([receive_signal/2, with_timeout/2]).\n\
             receive_signal(<<\"signoff\">>, _Config) ->\n\
             {error, <<\"cancelled:withdrawn\">>}.\n\
             with_timeout(_Deadline, Operation) -> {ok, Operation()}.\n",
            |result| result.starts_with("{error,"),
        ),
        (
            "engine",
            "-module(aion_flow_ffi).\n-export([with_timeout/2]).\n\
             with_timeout(_Deadline, _Operation) -> {error, <<\"engine down\">>}.\n",
            |result| result.starts_with("{error,") && result.contains("engine down"),
        ),
    ];
    for (label, ffi_body, holds) in arms {
        let mut ebins = base_ebins.clone();
        ebins.push(wait_ffi_ebin(label, ffi_body)?);
        let vm = build_vm(&ebins, std::slice::from_ref(&direct_bytes))?;
        let direct_result = vm.call0("wait_timeout", "awl$rt_execute")?;
        let reference_result = vm.call0("ref_wait_timeout", "awl_rt_execute")?;
        assert_eq!(direct_result, reference_result, "{label} arm parity");
        assert!(
            holds(&direct_result),
            "{label} arm semantics drifted: {direct_result}"
        );
    }
    Ok(())
}
