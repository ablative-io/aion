//! Focused selection regressions for outcome guards, S17 control tails, and
//! the loop bursts (Increment / untagged tuple / self tail call).

use std::fs;
use std::path::{Path, PathBuf};

use beamr::atom::AtomTable;
use beamr::loader::decode::{BifOp, Instruction, Operand};
use beamr::loader::load::load_beam_chunks;

use crate::mir::{lower, print_mir};

use super::select;

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rev2")
        .join(relative)
}

fn lower_fixture(path: &Path) -> Result<crate::mir::MirModule, Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)?;
    let document = crate::parse(&source)?;
    Ok(lower(&document, path.parent())?)
}

#[test]
fn outcome_guard_decision_tree_uses_real_failure_labels() -> Result<(), Box<dyn std::error::Error>>
{
    let path = fixture("step-bodies/valid/predicates_and_operators.awl");
    let module = lower_fixture(&path)?;
    let mir = print_mir(&module);
    for shape in [" = cmp ", "if is_true"] {
        assert!(
            mir.contains(shape),
            "short-circuit decision tree missed `{shape}`:\n{mir}"
        );
    }
    assert!(
        !mir.contains(" = boolop "),
        "guard still lowered eagerly:\n{mir}"
    );

    let bytes = select(&module)?;
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&bytes, &atoms)?;
    let failures: Vec<u32> = parsed
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::Comparison {
                fail: Operand::Label(label),
                ..
            } => Some(*label),
            _ => None,
        })
        .collect();
    assert!(
        !failures.is_empty(),
        "guard selection emitted no comparisons"
    );
    assert!(
        failures.iter().all(|label| *label != 0),
        "a guard comparison used trap label zero: {failures:?}"
    );
    Ok(())
}

#[test]
fn short_circuit_optional_selects_checked_assert_some() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("schema-doors/valid/short_circuit_optional.awl");
    let module = lower_fixture(&path)?;
    let mir = print_mir(&module);
    assert!(
        mir.contains(" = assert_some "),
        "missing AssertSome:\n{mir}"
    );

    let bytes = select(&module)?;
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&bytes, &atoms)?;
    let checks: Vec<u32> = parsed
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::IsTaggedTuple {
                fail: Operand::Label(label),
                arity: Operand::Unsigned(2),
                ..
            } => Some(*label),
            _ => None,
        })
        .collect();
    assert!(
        checks.iter().any(|label| *label != 0),
        "AssertSome emitted no explicit failure label: {checks:?}"
    );
    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Badmatch { .. })),
        "AssertSome failure did not end in an explicit badmatch trap"
    );
    Ok(())
}

/// The Increment burst: `gc_bif2` against a real `erlang:'+'/2` `ImpT` row
/// (beamr's `Bif` resolves through the import table), with fail label 0 —
/// DELIBERATE here, so a non-integer raises `badarith` exactly like Gleam's
/// `+`, instead of branching to a trap label.
#[test]
fn loop_counter_selects_a_gc_bif2_against_erlang_plus() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("loop-outcomes/valid/loop_counting_until_max.awl");
    let module = lower_fixture(&path)?;
    let mir = print_mir(&module);
    for shape in [" = increment ", " = tuple([", "tail_local poll_loop_0("] {
        assert!(
            mir.contains(shape),
            "loop lowering missed `{shape}`:\n{mir}"
        );
    }

    let bytes = select(&module)?;
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&bytes, &atoms)?;
    let bif = parsed
        .instructions
        .iter()
        .find_map(|instruction| match instruction {
            Instruction::Bif {
                op: BifOp::GcBif2,
                operands,
            } => Some(operands.clone()),
            _ => None,
        })
        .ok_or("no gc_bif2 selected for the loop counter")?;
    let [fail, _live, import, _lhs, rhs, _dst] = bif.as_slice() else {
        return Err(format!("gc_bif2 operand shape unexpected: {bif:?}").into());
    };
    assert!(
        matches!(fail, Operand::Label(0)),
        "counter arithmetic must raise badarith (fail label 0), got {fail:?}"
    );
    assert!(
        matches!(rhs, Operand::Integer(1)),
        "the increment step must be exactly 1, got {rhs:?}"
    );
    let Operand::Unsigned(index) = import else {
        return Err(format!("gc_bif2 import operand is not an index: {import:?}").into());
    };
    let entry = parsed
        .imports
        .get(usize::try_from(*index)?)
        .ok_or("gc_bif2 import index out of range")?;
    let module_name = atoms.resolve(entry.module).unwrap_or_default();
    let function_name = atoms.resolve(entry.function).unwrap_or_default();
    assert_eq!(
        (module_name, function_name, entry.arity),
        ("erlang", "+", 2),
        "the counter BIF must be erlang:'+'/2"
    );
    Ok(())
}

/// Carried BC-2b-5 fix: `let assert [a, b] = subject` must badmatch the
/// SUBJECT list, not whichever walked tail the unroll left in X0 — an
/// overlong/too-short/non-list mismatch reports the whole subject, exactly
/// like the Gleam source form. Structurally: the fail block reloads the
/// subject's Y home into X0 immediately before the `badmatch`.
#[test]
fn assert_list_badmatch_traps_the_subject_list() -> Result<(), Box<dyn std::error::Error>> {
    use crate::mir::{
        Block, FlowFn, FnOrigin, FnRef, MirFn, MirModule, Span, Stmt, Tail, TyDesc, Value, Var,
    };
    let flow = MirFn::Flow(FlowFn {
        origin: FnOrigin::Execute,
        name: "execute".to_owned(),
        params: vec![Var(0)],
        param_tys: vec![TyDesc::Nil],
        ret_ty: TyDesc::Nil,
        body: Block {
            stmts: vec![Stmt::AssertList {
                binds: vec![Some(Var(1)), Some(Var(2))],
                list: Var(0),
                span: Span::zero(),
            }],
            tail: Tail::Return(Value::Var(Var(1))),
        },
        span: Span::zero(),
        degraded_parallel: false,
    });
    let module = MirModule {
        name: "al".to_owned(),
        source: "al.awl".to_owned(),
        atoms: Vec::new(),
        literals: Vec::new(),
        exports: vec![FnRef(0)],
        functions: vec![flow],
        types: Vec::new(),
    };
    let bytes = select(&module)?;
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&bytes, &atoms)?;
    let badmatch = parsed
        .instructions
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Badmatch { .. }))
        .ok_or("AssertList emitted no badmatch trap")?;
    // The subject list is the fn's only param, homed in Y0; the fail block
    // must reload it over the walked tail before trapping.
    assert!(
        matches!(
            parsed.instructions.get(badmatch.wrapping_sub(1)),
            Some(Instruction::Move {
                source: Operand::Y(0),
                destination: Operand::X(0),
            })
        ),
        "badmatch does not reload the subject list from Y0: {:?}",
        &parsed.instructions[badmatch.saturating_sub(2)..=badmatch]
    );
    assert!(
        matches!(
            parsed.instructions.get(badmatch),
            Some(Instruction::Badmatch {
                value: Operand::X(0)
            })
        ),
        "badmatch operand is not the reloaded subject"
    );
    Ok(())
}

#[test]
fn enum_total_tail_selects_with_explicit_case_end_trap() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("loop-outcomes/valid/enum_when_totality.awl");
    let module = lower_fixture(&path)?;
    let mir = print_mir(&module);
    assert!(
        mir.contains("  select "),
        "enum-total outcomes did not become a tail"
    );

    let bytes = select(&module)?;
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&bytes, &atoms)?;
    let select_failures: Vec<u32> = parsed
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::SelectVal {
                fail: Operand::Label(label),
                ..
            } => Some(*label),
            _ => None,
        })
        .collect();
    // Two enum select bursts since BC-2b-5: the outcome-total tail AND the
    // enum codec's `_to_json` case over the variant atoms.
    assert_eq!(select_failures.len(), 2, "expected two enum select bursts");
    for label in &select_failures {
        assert_ne!(*label, 0, "enum select used trap label zero");
    }
    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::CaseEnd { .. })),
        "enum mismatch did not end in an explicit case trap"
    );
    Ok(())
}
