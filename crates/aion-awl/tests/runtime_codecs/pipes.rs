//! Execution-parity proofs for the pipe forms landed on the direct path:
//! combinator stages (filter/sort/map/count + post-stage any/all) and the
//! pipe child stage. Direct-selected modules and reference-emitted
//! (gleam-built) modules run the SAME driver values; results must match byte
//! for byte, with semantic pins on sort order/stability and the child output
//! surviving the strict outcome-envelope decode.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl::mir::{Block, FnRef, MirModule, Tail, Value, lower, select};
use aion_awl::parse;

use super::drivers::{Body, atom_ref, lit_ref, push_driver};
use super::harness::{build_vm, gleam_build, manifest_dir, reference_module_at, scratch_build_dir};

type TestResult = Result<(), Box<dyn Error>>;

fn parity_fixture(name: &str) -> PathBuf {
    manifest_dir().join("tests/fixtures/parity").join(name)
}

/// Lower a parity document (they live outside the rev2 fixture tree, so the
/// BC-3 oracle and the COVERED ratchet keep their fixed denominator).
fn lowered_at(path: &Path) -> Result<MirModule, Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    let document = parse(&source)?;
    Ok(lower(&document, path.parent())?)
}

// ---- pipe combinators ------------------------------------------------------

const REF_COMBINATOR_DRIVER: &str = r#"
pub fn awl_rt_execute() {
  execute(PipeCombinatorsInput(findings: [
    Finding(title: "gamma", blocking: True, severity: 3),
    Finding(title: "alpha", blocking: False, severity: 1),
    Finding(title: "delta", blocking: True, severity: 2),
    Finding(title: "beta", blocking: True, severity: 2),
  ]))
}

pub fn awl_rt_execute_empty() {
  execute(PipeCombinatorsInput(findings: []))
}
"#;

/// Append the two production-`execute` drivers: four findings (duplicate
/// severity 2/2 pins sort stability) and the empty list (vacuous truth).
fn combinator_execute_drivers(module: &mut MirModule) {
    let input = atom_ref(module, "pipe_combinators_input");
    let finding = atom_ref(module, "finding");
    let true_atom = atom_ref(module, "true");
    let false_atom = atom_ref(module, "false");
    let gamma = lit_ref(module, "gamma");
    let alpha = lit_ref(module, "alpha");
    let delta = lit_ref(module, "delta");
    let beta = lit_ref(module, "beta");

    let mut body = Body::new();
    let rows = [
        (gamma, true_atom, 3),
        (alpha, false_atom, 1),
        (delta, true_atom, 2),
        (beta, true_atom, 2),
    ];
    let mut values = Vec::new();
    for (title, blocking, severity) in rows {
        let item = body.record(
            finding,
            vec![
                Value::Lit(title),
                Value::Atom(blocking),
                Value::Int(severity),
            ],
        );
        values.push(Value::Var(item));
    }
    let findings = body.list(values);
    let input_value = body.record(input, vec![Value::Var(findings)]);
    let result = body.call_local(FnRef(2), vec![Value::Var(input_value)]);
    push_driver(
        module,
        "awl$rt_execute",
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(result)),
        },
    );

    let mut body = Body::new();
    let findings = body.list(Vec::new());
    let input_value = body.record(input, vec![Value::Var(findings)]);
    let result = body.call_local(FnRef(2), vec![Value::Var(input_value)]);
    push_driver(
        module,
        "awl$rt_execute_empty",
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(result)),
        },
    );
}

#[test]
fn pipe_combinators_execute_with_reference_parity() -> TestResult {
    let path = parity_fixture("pipe_combinators.awl");
    let reference = reference_module_at(&path, REF_COMBINATOR_DRIVER)?;
    let ebins = gleam_build(&[("ref_pipe_combinators", &reference)])?;

    let mut direct = lowered_at(&path)?;
    combinator_execute_drivers(&mut direct);
    let direct_bytes = select(&direct)?;
    let vm = build_vm(&ebins, &[direct_bytes])?;

    let direct_full = vm.call0("pipe_combinators", "awl$rt_execute")?;
    let reference_full = vm.call0("ref_pipe_combinators", "awl_rt_execute")?;
    assert_eq!(direct_full, reference_full, "populated execute parity");
    assert!(
        direct_full.starts_with("{ok,"),
        "populated execute did not complete: {direct_full}"
    );
    // Blockers are [gamma(3), delta(2), beta(2)]; severity-sorted titles must
    // be [delta, beta, gamma] — the 2/2 duplicate keeps input order (sort
    // stability parity). Field order puts blocker_titles first.
    let delta_first = direct_full.find("delta").ok_or("missing delta")?;
    let beta_first = direct_full.find("beta").ok_or("missing beta")?;
    let gamma_first = direct_full.find("gamma").ok_or("missing gamma")?;
    assert!(
        delta_first < beta_first && beta_first < gamma_first,
        "severity sort order/stability drifted: {direct_full}"
    );
    // The alpha-sorted titles follow: [beta, delta, gamma].
    let alpha_section = &direct_full[gamma_first..];
    let beta_alpha = alpha_section
        .find("beta")
        .ok_or("missing alpha-sorted beta")?;
    let delta_alpha = alpha_section
        .find("delta")
        .ok_or("missing alpha-sorted delta")?;
    assert!(
        beta_alpha < delta_alpha,
        "alphabetical sort order drifted: {direct_full}"
    );

    let direct_empty = vm.call0("pipe_combinators", "awl$rt_execute_empty")?;
    let reference_empty = vm.call0("ref_pipe_combinators", "awl_rt_execute_empty")?;
    assert_eq!(direct_empty, reference_empty, "empty execute parity");
    assert!(
        direct_empty.starts_with("{ok,"),
        "empty execute did not complete: {direct_empty}"
    );
    // Vacuous truth: `any` is false, `all` is true on the empty list.
    assert!(
        direct_empty.contains("false") && direct_empty.contains("true"),
        "empty-list quantifiers lost vacuous truth: {direct_empty}"
    );
    Ok(())
}

// ---- pipe child stage ------------------------------------------------------

const ESSAY_JSON: &str = r#"{"essay":"hello"}"#;

const REF_PIPE_CHILD_DRIVER: &str = r#"
pub fn awl_rt_run() {
  run(dynamic.string("{\"essay\":\"hello\"}"))
}
"#;

/// Build the production FFI namespace for the pipe-child proof: `spawn_child`
/// executes the generated child's exported `run/1` on the exact encoded input
/// and records its `ok:`-prefixed encoded output verbatim (the production
/// envelope contract proven by `harness::child_host_ebin`), plus the
/// workflow-sized heap runner for the parent driver.
fn pipe_child_host_ebin(
    label: &str,
    parent_module: &str,
    run_fn: &str,
    child_module: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let dir = scratch_build_dir(&format!("pipe_child_host_{label}"));
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
            "pipe child host erlc failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(dir)
}

#[test]
fn pipe_child_stage_executes_with_reference_parity() -> TestResult {
    let parent_path = parity_fixture("pipe_child.awl");
    let child_path = parity_fixture("score_essay.awl");
    let ref_parent = reference_module_at(&parent_path, REF_PIPE_CHILD_DRIVER)?;
    let ref_child = reference_module_at(&child_path, "")?;
    let ebins = gleam_build(&[
        ("ref_pipe_child", &ref_parent),
        ("ref_score_essay", &ref_child),
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
    direct_ebins.push(pipe_child_host_ebin(
        "direct",
        "pipe_child",
        "'awl$rt_run'",
        "score_essay",
    )?);
    let direct_vm = build_vm(&direct_ebins, &direct_modules)?;
    let direct = direct_vm.call0_large("pipe_child", "awl$rt_run")?;

    let mut reference_ebins = ebins;
    reference_ebins.push(pipe_child_host_ebin(
        "reference",
        "ref_pipe_child",
        "awl_rt_run",
        "ref_score_essay",
    )?);
    let reference_vm = build_vm(&reference_ebins, &[])?;
    let reference = reference_vm.call0_large("ref_pipe_child", "awl_rt_run")?;

    assert_eq!(direct, reference, "pipe child stage parity");
    assert!(
        direct.starts_with("{ok,"),
        "parent did not complete: {direct}"
    );
    assert!(
        direct.contains("scored: hello"),
        "the child output did not survive the strict envelope decode and \
         post-child projection: {direct}"
    );
    Ok(())
}
