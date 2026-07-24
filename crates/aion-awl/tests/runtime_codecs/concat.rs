//! Runtime parity proof for general, left-associated string concatenation.

use std::error::Error;

use aion_awl::mir::{Block, MirModule, Tail, Value, select};
use beamr::process::ExitReason;

use super::drivers::{Body, atom_ref, fn_by_name, lit_ref, lowered, push_driver};
use super::harness::{build_vm, gleam_build, reference_module};

type TestResult = Result<(), Box<dyn Error>>;

const REFERENCE_DRIVER: &str = r#"
pub fn awl_rt_concat() -> #(String, Bool) {
  let assert Ok(ProvisionedOutcome(Provisioned(path: path))) =
    execute(GeneralConcatInput(config: Config(repo_root: "/srv/repo")))
  #(path, True)
}
"#;

fn direct_driver(module: &mut MirModule) -> Result<(), Box<dyn Error>> {
    let execute = fn_by_name(module, "execute")?;
    let input = atom_ref(module, "general_concat_input");
    let config = atom_ref(module, "config");
    let true_atom = atom_ref(module, "true");
    let repo_root = lit_ref(module, "/srv/repo");

    let mut body = Body::new();
    let config_value = body.record(config, vec![Value::Lit(repo_root)]);
    let input_value = body.record(input, vec![Value::Var(config_value)]);
    let result = body.call_local(execute, vec![Value::Var(input_value)]);
    let outcome = body.field(result, 1);
    let provisioned = body.field(outcome, 1);
    let path = body.field(provisioned, 1);
    let returned = body.tuple(vec![Value::Var(path), Value::Atom(true_atom)]);
    push_driver(
        module,
        "awl$rt_concat",
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(returned)),
        },
    );
    Ok(())
}

#[test]
fn general_concat_executes_with_emitter_byte_parity() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let reference = reference_module("step-bodies/valid/general_concat.awl", REFERENCE_DRIVER)?;
    let ebins = gleam_build(&[("ref_general_concat", &reference)])?;

    let mut direct = lowered("step-bodies/valid/general_concat.awl")?;
    direct_driver(&mut direct)?;
    let direct_bytes = select(&direct)?;
    let vm = build_vm(&ebins, &[direct_bytes])?;

    let (direct_reason, direct_term, direct_path) =
        vm.call0_raw("general_concat", "awl$rt_concat")?;
    let (reference_reason, reference_term, reference_path) =
        vm.call0_raw("ref_general_concat", "awl_rt_concat")?;
    assert_eq!(direct_reason, ExitReason::Normal, "direct: {direct_term}");
    assert_eq!(
        reference_reason,
        ExitReason::Normal,
        "reference: {reference_term}"
    );

    let direct_path = direct_path.ok_or("direct driver returned no path bytes")?;
    let reference_path = reference_path.ok_or("reference driver returned no path bytes")?;
    let expected = b"/srv/repo/.yggdrasil-worktrees/dev-brief/test-workflow-id";
    assert_eq!(direct_path.as_slice(), expected);
    assert_eq!(reference_path.as_slice(), expected);
    assert_eq!(direct_path, reference_path);
    assert_eq!(direct_term, reference_term);
    Ok(())
}
