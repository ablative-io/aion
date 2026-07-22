//! Deliverable 3: the ABI contract tests (`AWL-BC-IR.md` §7).
//!
//! These pin the rows that the differential exercises but does not by itself
//! nail down at the byte level, decoding the direct-`select`ed `.beam` bytes
//! and asserting the loaded-module ABI directly:
//!
//! - IR-15 export set: exactly `definition/0`, `run/1`, `execute/1`; no
//!   `module_info/0,1`.
//! - IR-13/IR-14 entry ABI + calling convention: `run/1` is exported at arity
//!   one (the raw input payload term). The `{ok, ResultBinary}` /
//!   `{error, AwlErrorTerm}` return and verbatim `WorkflowCompleted.result`
//!   recording are proven end-to-end by the covered differential
//!   (`covered.rs`) — every completing fixture's result bytes are byte-identical
//!   across both backends.
//! - IR-12 module-name mangling: the direct module's own module atom equals the
//!   reference emitter's mangled entry-module name.
//! - IR-2 float literal byte-parity: a `Float` literal in the `LitT` chunk
//!   equals Rust's parse of the same source lexeme (the reference's parse).
//! - Writer contract chunk set/order: `AtU8, Code, ImpT, ExpT, FunT, LitT,
//!   StrT, Line` in canonical order, optional chunks only when non-empty, and
//!   no other chunk (no `int_code_end` terminator, no `module_info`).
//!
//! Rows already pinned inside `aion-awl` are referenced, not duplicated:
//! IR-5/6/7/8 (Option `{some,V}`/`none`, Result `{ok,V}`/`{error,E}`, record
//! tuples, enum bare atoms) execute under `aion-awl/tests/runtime_codecs.rs`;
//! IR-10 SDK constructor atoms are pinned at `src/mir/lower/stmts.rs`; the
//! export set is built at `src/mir/select/assemble.rs`.

use beamr::atom::AtomTable;
use beamr::loader::decode::Literal;
use beamr::loader::load::ParsedModule;
use beamr::loader::{load_beam_chunks, parse_beam_chunks};

use aion_awl::emit_artifact_in;
use aion_awl::mir::{lower, select};

use crate::fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The canonical writer chunk order (`encode/container.rs`). Optional chunks
/// (everything after `Code`) appear only when non-empty, but always in this
/// relative order.
const CANONICAL_CHUNKS: &[&[u8; 4]] = &[
    b"AtU8", b"Code", b"ImpT", b"ExpT", b"FunT", b"LitT", b"StrT", b"Line",
];

/// Lowers + selects a covered fixture to its direct `.beam` bytes.
fn direct_bytes(name: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let loaded = fixtures::load(name)?;
    let module = lower(&loaded.document, Some(loaded.dir.as_path()))?;
    Ok(select(&module)?)
}

/// Decodes direct bytes into `(atom table, parsed module)` so export/atom names
/// resolve against the same table the decode interned them into.
fn decode(bytes: &[u8]) -> Result<(AtomTable, ParsedModule), Box<dyn std::error::Error>> {
    let table = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(bytes, &table)?;
    Ok((table, parsed))
}

/// IR-15: the export set is EXACTLY `definition/0`, `run/1`, `execute/1`, with
/// no `module_info/0,1` and nothing else.
#[test]
fn ir15_export_set_is_exactly_definition_run_execute() -> TestResult {
    let bytes = direct_bytes("flagship/valid/awl_hello")?;
    let (table, parsed) = decode(&bytes)?;
    let mut exports: Vec<(String, u8)> = parsed
        .exports
        .iter()
        .map(|export| {
            (
                table.resolve(export.function).unwrap_or("<?>").to_owned(),
                export.arity,
            )
        })
        .collect();
    exports.sort();
    assert_eq!(
        exports,
        vec![
            (String::from("definition"), 0),
            (String::from("execute"), 1),
            (String::from("run"), 1),
        ],
        "IR-15 export set drifted"
    );
    assert!(
        !exports.iter().any(|(name, _)| name == "module_info"),
        "IR-15: module_info must never be exported"
    );
    Ok(())
}

/// IR-13/IR-14: `run/1` is exported at arity one — the raw input payload term
/// arrives in a single argument.
#[test]
fn ir13_entry_run_is_exported_at_arity_one() -> TestResult {
    for name in [
        "flagship/valid/awl_hello",
        "step-bodies/valid/workflow_id",
        "header-types/valid/enum",
    ] {
        let bytes = direct_bytes(name)?;
        let (table, parsed) = decode(&bytes)?;
        let run = parsed
            .exports
            .iter()
            .find(|export| table.resolve(export.function) == Some("run") && export.arity == 1);
        assert!(
            run.is_some(),
            "{name}: run/1 must be exported (IR-13/IR-14)"
        );
    }
    Ok(())
}

/// IR-12: the direct module's own module atom equals the reference emitter's
/// mangled entry-module name, across single- and multi-word workflow names.
#[test]
fn ir12_module_atom_matches_reference_mangling() -> TestResult {
    for name in [
        "flagship/valid/awl_hello",
        "header-types/valid/signal_wait",
        "loop-outcomes/valid/float_threshold_guard",
    ] {
        let loaded = fixtures::load(name)?;
        let artifact = emit_artifact_in(&loaded.document, &loaded.dir)?;
        let module = lower(&loaded.document, Some(loaded.dir.as_path()))?;
        let bytes = select(&module)?;
        let (table, parsed) = decode(&bytes)?;
        let module_atom = table.resolve(parsed.name).unwrap_or("<?>");
        assert_eq!(
            module_atom, artifact.entry_module,
            "{name}: IR-12 direct module atom must equal the reference mangled entry module"
        );
    }
    Ok(())
}

/// IR-2: a `Float` literal is carried in `LitT` with byte-parity to Rust's
/// parse of the same source lexeme (`0.5` in `float_threshold_guard`).
#[test]
fn ir2_float_literal_matches_the_reference_parse() -> TestResult {
    let bytes = direct_bytes("loop-outcomes/valid/float_threshold_guard")?;
    let (_table, parsed) = decode(&bytes)?;
    let expected: f64 = "0.5".parse()?;
    let mut floats = Vec::new();
    for literal in &parsed.literals {
        collect_floats(literal, &mut floats);
    }
    assert!(
        floats
            .iter()
            .any(|value| value.to_bits() == expected.to_bits()),
        "IR-2: the 0.5 float literal must appear in LitT with byte-identical bits; found {floats:?}"
    );
    Ok(())
}

/// Recursively collects every `Float` value nested in a literal.
fn collect_floats(literal: &Literal, into: &mut Vec<f64>) {
    match literal {
        Literal::Float(value) => into.push(*value),
        Literal::Tuple(items) | Literal::List(items, _) => {
            for item in items {
                collect_floats(item, into);
            }
        }
        Literal::Map(pairs) => {
            for (key, value) in pairs {
                collect_floats(key, into);
                collect_floats(value, into);
            }
        }
        _ => {}
    }
}

/// Writer contract: the chunk set is a subset of the canonical set, in
/// canonical order, with `AtU8` and `Code` always first, and no foreign chunk
/// (no `int_code_end` terminator, no `module_info` machinery).
#[test]
fn writer_contract_chunk_set_and_order() -> TestResult {
    for name in [
        "flagship/valid/awl_hello",
        "loop-outcomes/valid/float_threshold_guard",
        "step-bodies/valid/workflow_id",
    ] {
        let bytes = direct_bytes(name)?;
        let chunks = parse_beam_chunks(&bytes)?;
        let names: Vec<[u8; 4]> = chunks.iter().map(|(id, _)| *id).collect();
        assert!(
            names.first() == Some(b"AtU8") && names.get(1) == Some(b"Code"),
            "{name}: AtU8 then Code must lead the container, got {:?}",
            render_chunks(&names)
        );
        for id in &names {
            assert!(
                CANONICAL_CHUNKS.contains(&id),
                "{name}: foreign chunk {:?} in the container",
                String::from_utf8_lossy(id)
            );
        }
        assert_eq!(
            names,
            canonical_subsequence(&names),
            "{name}: chunks out of canonical order: {:?}",
            render_chunks(&names)
        );
    }
    Ok(())
}

/// Returns `names` reordered onto the canonical chunk order; equality with the
/// input proves the input already respects that order (and has no duplicates).
fn canonical_subsequence(names: &[[u8; 4]]) -> Vec<[u8; 4]> {
    CANONICAL_CHUNKS
        .iter()
        .filter(|canonical| names.iter().any(|id| &id == *canonical))
        .map(|canonical| **canonical)
        .collect()
}

/// Renders chunk ids for a readable assertion message.
fn render_chunks(names: &[[u8; 4]]) -> Vec<String> {
    names
        .iter()
        .map(|id| String::from_utf8_lossy(id).into_owned())
        .collect()
}
