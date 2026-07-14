//! Execution-grade AWL parent → AWL child wire proof.
//!
//! Both generated backends run the real parent `run/1` host. The production
//! child FFI invokes the matching generated child's own `run/1`, records those
//! encoded outcome bytes verbatim, and lets the SDK's normal child await path
//! decode them through the codec carried on the parent handle.

use std::error::Error;

use aion_awl::mir::{Block, FnRef, RuntimeFn, Stmt, Tail, Value, select};

use super::drivers::{Body, atom_ref, decode_is_error, fn_by_name, lit_ref, lowered, push_driver};
use super::harness::{build_vm, child_host_ebin, gleam_build, reference_module};

const PARENT: &str = "dag-fork/valid/child_collection_fork.awl";
const CHILD: &str = "dag-fork/valid/sit_one.awl";
const INPUT_JSON: &str = r#"{"pack_revision":"r1","sittings":[{"harness":"h1","model":"m1","effort":"e1"},{"harness":"h2","model":"m2","effort":"e2"}]}"#;
const BARE_ROW_JSON: &str =
    r#"{"spec":{"harness":"legacy","model":"bare","effort":"none"},"first_try":true}"#;
const NEUTRAL_ROW_JSON: &str = r#"{"outcome":"child","payload":{"spec":{"harness":"h1","model":"m1","effort":"e1"},"first_try":true}}"#;

const REF_PARENT_DRIVER: &str = r#"
pub fn awl_rt_run() {
  run(dynamic.string("{\"pack_revision\":\"r1\",\"sittings\":[{\"harness\":\"h1\",\"model\":\"m1\",\"effort\":\"e1\"},{\"harness\":\"h2\",\"model\":\"m2\",\"effort\":\"e2\"}]}"))
}

pub fn awl_rt_bare_decode_fails() -> Bool {
  case awl_child_output_sitting_row_codec().decode("{\"spec\":{\"harness\":\"legacy\",\"model\":\"bare\",\"effort\":\"none\"},\"first_try\":true}") {
    Ok(_) -> False
    Error(_) -> True
  }
}

pub fn awl_rt_neutral_roundtrip() -> #(String, Bool) {
  let value = SittingRow(
    spec: SittingSpec(harness: "h1", model: "m1", effort: "e1"),
    first_try: True,
  )
  let codec = awl_child_output_sitting_row_codec()
  let encoded = codec.encode(value)
  #(encoded, codec.decode(encoded) == Ok(value))
}

pub fn awl_rt_bare_child_error() {
  let input = json.object([
    #("spec", sitting_spec_to_json(SittingSpec(
      harness: "h1", model: "m1", effort: "e1",
    ))),
    #("pack_revision", json.string("r1")),
  ])
  case workflow.spawn(
    "sit_one",
    fn(_: json.Json) { Error(awl_error.AwlChildFailed("child workflow body runs in its own execution")) },
    input,
    awlc.json_value(),
    awl_child_output_sitting_row_codec(),
    awl_error.codec(),
  ) {
    Ok(handle) -> child.await(handle)
    Error(error.EngineFailure(message)) -> Error(error.ChildEngineFailure(message: message))
  }
}
"#;

fn production_run_driver(module: &mut aion_awl::mir::MirModule) {
    let input = lit_ref(module, INPUT_JSON);
    let mut body = Body::new();
    let result = body.call_local(FnRef(0), vec![Value::Lit(input)]);
    push_driver(
        module,
        "awl$rt_run",
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(result)),
        },
    );
}

fn child_codec_drivers(module: &mut aion_awl::mir::MirModule) -> Result<(), Box<dyn Error>> {
    let codec = fn_by_name(module, "awl_child_output_sitting_row_codec")?;
    let spawn_fold = fn_by_name(module, "fan_out_fork_0")?;
    let bare = lit_ref(module, BARE_ROW_JSON);
    let error = atom_ref(module, "error");
    let true_atom = atom_ref(module, "true");
    let false_atom = atom_ref(module, "false");
    let block = decode_is_error(codec, bare, error, true_atom, false_atom);
    push_driver(module, "awl$rt_bare_decode_fails", block);

    let sitting_spec = atom_ref(module, "sitting_spec");
    let sitting_row = atom_ref(module, "sitting_row");
    let ok = atom_ref(module, "ok");
    let h1 = lit_ref(module, "h1");
    let m1 = lit_ref(module, "m1");
    let e1 = lit_ref(module, "e1");
    let mut body = Body::new();
    let spec = body.record(
        sitting_spec,
        vec![Value::Lit(h1), Value::Lit(m1), Value::Lit(e1)],
    );
    let row = body.record(sitting_row, vec![Value::Var(spec), Value::Atom(true_atom)]);
    let block = body.roundtrip_tail(codec, Value::Var(row), ok);
    push_driver(module, "awl$rt_neutral_roundtrip", block);

    let pack = lit_ref(module, "r1");
    let mut body = Body::new();
    let spec = body.record(
        sitting_spec,
        vec![Value::Lit(h1), Value::Lit(m1), Value::Lit(e1)],
    );
    let spawned = body.call_local(
        spawn_fold,
        vec![Value::Nil, Value::Var(spec), Value::Lit(pack)],
    );
    let handles = body.field(spawned, 1);
    let handle = body.var();
    body.stmts.push(Stmt::AssertList {
        binds: vec![Some(handle)],
        list: handles,
        span: aion_awl::mir::Span { line: 0, column: 0 },
    });
    let waited = body.call_rt(RuntimeFn::ChildAwait, vec![Value::Var(handle)]);
    push_driver(
        module,
        "awl$rt_bare_child_error",
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(waited)),
        },
    );
    Ok(())
}

fn direct_modules() -> Result<(Vec<u8>, Vec<u8>), Box<dyn Error>> {
    let mut parent = lowered(PARENT)?;
    production_run_driver(&mut parent);
    child_codec_drivers(&mut parent)?;
    let child = lowered(CHILD)?;
    Ok((select(&parent)?, select(&child)?))
}

fn reference_ebins() -> Result<Vec<std::path::PathBuf>, Box<dyn Error>> {
    let parent = reference_module(PARENT, REF_PARENT_DRIVER)?;
    let child = reference_module(CHILD, "")?;
    gleam_build(&[
        ("ref_child_collection_fork", &parent),
        ("ref_sit_one", &child),
    ])
}

fn with_host(
    mut ebins: Vec<std::path::PathBuf>,
    label: &str,
    child_module: &str,
    bare: bool,
) -> Result<Vec<std::path::PathBuf>, Box<dyn Error>> {
    ebins.push(child_host_ebin(label, child_module, bare)?);
    Ok(ebins)
}

fn assert_input_order(result: &str) -> Result<(), Box<dyn Error>> {
    let h1 = result.find("h1").ok_or("parent result omitted h1")?;
    let h2 = result.find("h2").ok_or("parent result omitted h2")?;
    assert!(h1 < h2, "child join lost input order: {result}");
    Ok(())
}

#[test]
fn real_child_outcome_envelope_completes_parent_fold_on_both_backends() -> Result<(), Box<dyn Error>>
{
    let reference = reference_ebins()?;
    let (direct_parent, direct_child) = direct_modules()?;

    let reference_ebins = with_host(
        reference.clone(),
        "reference_envelope",
        "ref_sit_one",
        false,
    )?;
    let reference_vm = build_vm(&reference_ebins, &[])?;
    let emitted = reference_vm.call0_large("ref_child_collection_fork", "awl_rt_run")?;

    let direct_ebins = with_host(reference, "direct_envelope", "sit_one", false)?;
    let direct_vm = build_vm(&direct_ebins, &[direct_parent, direct_child])?;
    let direct = direct_vm.call0_large("child_collection_fork", "awl$rt_run")?;

    assert_eq!(direct, emitted, "production parent/child host parity");
    assert!(
        direct.starts_with("{ok,"),
        "parent did not complete: {direct}"
    );
    assert_input_order(&direct)?;
    Ok(())
}

#[test]
fn child_output_codec_is_strict_and_symmetric_on_both_backends() -> Result<(), Box<dyn Error>> {
    let reference = reference_ebins()?;
    let (direct_parent, direct_child) = direct_modules()?;
    let host = with_host(reference.clone(), "direct_strict", "sit_one", false)?;
    let direct_vm = build_vm(&host, &[direct_parent.clone(), direct_child.clone()])?;
    let reference_host = with_host(reference.clone(), "reference_strict", "ref_sit_one", false)?;
    let reference_vm = build_vm(&reference_host, &[])?;

    let (direct_encoded, direct_roundtrip) =
        direct_vm.roundtrip_large("child_collection_fork", "awl$rt_neutral_roundtrip")?;
    let (reference_encoded, reference_roundtrip) =
        reference_vm.roundtrip_large("ref_child_collection_fork", "awl_rt_neutral_roundtrip")?;
    assert_eq!(direct_encoded, NEUTRAL_ROW_JSON);
    assert_eq!(reference_encoded, NEUTRAL_ROW_JSON);
    assert_eq!(direct_encoded, reference_encoded);
    assert!(direct_roundtrip && reference_roundtrip);
    assert_eq!(
        direct_vm.call0_large("child_collection_fork", "awl$rt_bare_decode_fails")?,
        "true"
    );
    assert_eq!(
        reference_vm.call0_large("ref_child_collection_fork", "awl_rt_bare_decode_fails")?,
        "true"
    );

    let direct_bare_host = with_host(reference.clone(), "direct_bare", "sit_one", true)?;
    let direct_bare_vm = build_vm(&direct_bare_host, &[direct_parent, direct_child])?;
    let direct_typed =
        direct_bare_vm.call0_large("child_collection_fork", "awl$rt_bare_child_error")?;
    let direct_error = direct_bare_vm.call0_large("child_collection_fork", "awl$rt_run")?;
    let reference_bare_host = with_host(reference, "reference_bare", "ref_sit_one", true)?;
    let reference_bare_vm = build_vm(&reference_bare_host, &[])?;
    let reference_typed =
        reference_bare_vm.call0_large("ref_child_collection_fork", "awl_rt_bare_child_error")?;
    let reference_error =
        reference_bare_vm.call0_large("ref_child_collection_fork", "awl_rt_run")?;
    assert_eq!(direct_typed, reference_typed);
    assert!(
        direct_typed.contains("child_output_decode_failed"),
        "bare result did not surface ChildOutputDecodeFailed: {direct_typed}"
    );
    assert_eq!(direct_error, reference_error);
    assert_eq!(
        direct_error,
        "{error, {awl_child_failed, <<\"child workflow failed\">>}}"
    );
    Ok(())
}
