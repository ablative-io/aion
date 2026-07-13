//! Focused selection regressions for outcome guards and S17 control tails.

use std::fs;
use std::path::{Path, PathBuf};

use beamr::atom::AtomTable;
use beamr::loader::decode::{Instruction, Operand};
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
    assert_eq!(select_failures.len(), 1, "expected one enum select burst");
    assert_ne!(select_failures[0], 0, "enum select used trap label zero");
    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::CaseEnd { .. })),
        "enum mismatch did not end in an explicit case trap"
    );
    Ok(())
}
