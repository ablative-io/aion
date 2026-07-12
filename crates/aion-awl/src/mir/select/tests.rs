//! BC-3 tests: the emit oracle (every checking fixture that lowers must emit
//! and pass all five loader layers) and per-shape unit tests keyed to the
//! §11.4 burst table (each hand-built body assembled + validated in isolation).

use std::fs;
use std::path::{Path, PathBuf};

use beamr::atom::AtomTable;
use beamr::loader::decode::Instruction;
use beamr::loader::load::ParsedModule;
use beamr::loader::load_beam_chunks;

use crate::mir::{AtomRef, JsonVal, ToJsonRef};
use crate::mir::{
    Block, FlowFn, FnOrigin, FnRef, Leaf, LitRef, LiveAfter, LowerError, MirFn, MirLiteral,
    MirModule, RuntimeFn, Span, Stmt, Tail, TyDesc, Value, Var, lower, select, verify,
};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn valid_fixtures() -> Vec<PathBuf> {
    let root = manifest_dir().join("tests/fixtures/rev2");
    let mut found = Vec::new();
    collect(&root, &mut found);
    found.sort();
    found
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "awl")
            && path
                .components()
                .any(|component| component.as_os_str() == "valid")
        {
            out.push(path);
        }
    }
}

/// The emit oracle (plan §3 BC-3 row): every `valid/` fixture that lowers to MIR
/// must emit `.beam` bytes that load AND validate through all five loader layers
/// (`select` self-gates), and every fixture that BC-2 refuses stays refused —
/// no fixture is silently skipped.
#[test]
fn every_lowered_fixture_emits_and_validates() -> Result<(), Box<dyn std::error::Error>> {
    let mut emitted = 0_usize;
    let mut refused = 0_usize;
    for fixture in valid_fixtures() {
        let source = fs::read_to_string(&fixture)?;
        let document = crate::parse(&source).map_err(|error| {
            format!(
                "valid fixture {} no longer parses: {error}",
                fixture.display()
            )
        })?;
        match lower(&document, fixture.parent()) {
            Ok(module) => {
                verify(&module)?;
                // `select` self-gates: an Ok return means the emitted module
                // loaded + `validate_module`d clean (the BC-3 oracle).
                select(&module).map_err(|error| {
                    format!("{} did not emit + validate: {error}", fixture.display())
                })?;
                emitted += 1;
            }
            Err(LowerError::Unsupported { .. } | LowerError::Planning { .. }) => refused += 1,
            Err(other) => return Err(Box::new(other)),
        }
    }
    assert!(
        emitted > 0,
        "no fixture emitted — the oracle proved nothing"
    );
    println!("BC-3 oracle: {emitted} fixtures emitted + validated, {refused} refused");
    Ok(())
}

/// Determinism (#218): the same MIR selects the same bytes every time.
#[test]
fn selection_is_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2/header-types/valid/minimal.awl");
    let source = fs::read_to_string(&path)?;
    let document = crate::parse(&source)?;
    let module = lower(&document, path.parent())?;
    let first = select(&module)?;
    let second = select(&module)?;
    assert_eq!(first, second, "select is not a pure function of the MIR");
    assert!(first.starts_with(b"FOR1"), "output is not a BEAM container");
    Ok(())
}

/// The `minimal` fixture's emitted module has the exact ABI structure: three
/// exports (run/1, definition/0, execute/1 — decision 12/IR-15), a `Line`-free
/// canonical chunk set, and the shell/region instruction alphabet actually
/// selected (`Allocate` frames, `CallExt` shells, `IsTaggedTuple` `TryBind`, `MakeFun`
/// closures, `PutTuple2` records).
#[test]
fn minimal_module_has_the_expected_abi_structure() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2/header-types/valid/minimal.awl");
    let source = fs::read_to_string(&path)?;
    let document = crate::parse(&source)?;
    let module = lower(&document, path.parent())?;
    let (parsed, table) = gate(&module)?;

    let exports: Vec<(String, u8)> = parsed
        .exports
        .iter()
        .map(|export| (name_of(&table, export.function), export.arity))
        .collect();
    assert!(exports.contains(&("run".to_owned(), 1)));
    assert!(exports.contains(&("definition".to_owned(), 0)));
    assert!(exports.contains(&("execute".to_owned(), 1)));
    assert_eq!(exports.len(), 3, "exactly the three catalog exports");
    assert!(
        !parsed
            .exports
            .iter()
            .any(|export| name_of(&table, export.function) == "module_info"),
        "no module_info export (decision 12)"
    );

    assert!(count(&parsed, |i| matches!(i, Instruction::Allocate { .. })) >= 1);
    assert!(
        count(&parsed, |i| matches!(i, Instruction::IsTaggedTuple { .. })) >= 1,
        "TryBind burst"
    );
    assert!(
        count(&parsed, |i| matches!(i, Instruction::MakeFun { .. })) >= 1,
        "codec/execute closures"
    );
    assert!(
        count(&parsed, |i| matches!(i, Instruction::PutTuple2 { .. })) >= 1,
        "record/outcome tuples"
    );
    assert!(
        count(&parsed, |i| matches!(i, Instruction::CallExtLast { .. })) >= 1,
        "shell tails"
    );
    assert!(!parsed.lambdas.is_empty(), "FunT carries the closures");
    Ok(())
}

// ---- per-shape unit tests (§11.4 rows, each in an isolated module) ----

fn sp() -> Span {
    Span { line: 0, column: 0 }
}

fn flow(name: &str, origin: FnOrigin, params: &[u32], stmts: Vec<Stmt>, tail: Tail) -> MirFn {
    let param_vars: Vec<Var> = params.iter().map(|index| Var(*index)).collect();
    let param_tys = vec![TyDesc::Nil; params.len()];
    MirFn::Flow(FlowFn {
        origin,
        name: name.to_owned(),
        params: param_vars,
        param_tys,
        ret_ty: TyDesc::Nil,
        body: Block { stmts, tail },
        span: sp(),
        degraded_parallel: false,
    })
}

fn module(
    name: &str,
    atoms: &[&str],
    literals: Vec<MirLiteral>,
    functions: Vec<MirFn>,
) -> MirModule {
    MirModule {
        name: name.to_owned(),
        source: format!("{name}.awl"),
        atoms: atoms.iter().map(|atom| (*atom).to_owned()).collect(),
        literals,
        exports: vec![FnRef(0)],
        functions,
        types: Vec::new(),
    }
}

type GateResult = Result<(ParsedModule, AtomTable), Box<dyn std::error::Error>>;

fn gate(module: &MirModule) -> GateResult {
    let bytes = select(module)?;
    let table = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&bytes, &table)?;
    Ok((parsed, table))
}

fn count(parsed: &ParsedModule, predicate: impl Fn(&Instruction) -> bool) -> usize {
    parsed
        .instructions
        .iter()
        .filter(|instruction| predicate(instruction))
        .count()
}

fn name_of(table: &AtomTable, atom: beamr::atom::Atom) -> String {
    table.resolve(atom).unwrap_or_default().to_owned()
}

/// §11.4 `Record` (tuple, var arg) + `Return` + framed exit.
#[test]
fn shape_record_tuple() -> Result<(), Box<dyn std::error::Error>> {
    let flow = flow(
        "execute",
        FnOrigin::Execute,
        &[0],
        vec![Stmt::RecordNew {
            dst: Var(1),
            tag: AtomRef(0),
            args: vec![Value::Var(Var(0))],
            span: sp(),
        }],
        Tail::Return(Value::Var(Var(1))),
    );
    let (parsed, _table) = gate(&module("rec", &["ok"], Vec::new(), vec![flow]))?;
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::PutTuple2 { .. })),
        1
    );
    assert!(count(&parsed, |i| matches!(i, Instruction::Deallocate { .. })) >= 1);
    Ok(())
}

/// §11.4 `Record` (zero-field ⇒ bare tag atom `move`).
#[test]
fn shape_record_zero_field() -> Result<(), Box<dyn std::error::Error>> {
    let flow = flow(
        "execute",
        FnOrigin::Execute,
        &[],
        vec![Stmt::RecordNew {
            dst: Var(0),
            tag: AtomRef(0),
            args: Vec::new(),
            span: sp(),
        }],
        Tail::Return(Value::Var(Var(0))),
    );
    let (parsed, _table) = gate(&module("rec0", &["done"], Vec::new(), vec![flow]))?;
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::PutTuple2 { .. })),
        0,
        "bare atom, no tuple"
    );
    Ok(())
}

/// §11.4 `FieldGet` (`get_tuple_element base, index`).
#[test]
fn shape_field_get() -> Result<(), Box<dyn std::error::Error>> {
    let flow = flow(
        "execute",
        FnOrigin::Execute,
        &[0],
        vec![Stmt::FieldGet {
            dst: Var(1),
            base: Value::Var(Var(0)),
            index: 1,
            span: sp(),
        }],
        Tail::Return(Value::Var(Var(1))),
    );
    let (parsed, _table) = gate(&module("fg", &[], Vec::new(), vec![flow]))?;
    let element = parsed
        .instructions
        .iter()
        .find_map(|instruction| match instruction {
            Instruction::GetTupleElement { index, .. } => Some(index.clone()),
            _ => None,
        });
    assert!(
        matches!(element, Some(beamr::loader::decode::Operand::Unsigned(1))),
        "element index is verbatim"
    );
    Ok(())
}

/// §11.4 `CallImport` (`call_ext` into a durable `RuntimeFn`).
#[test]
fn shape_call_import() -> Result<(), Box<dyn std::error::Error>> {
    let flow = flow(
        "execute",
        FnOrigin::Execute,
        &[0],
        vec![Stmt::CallRt {
            dst: Some(Var(1)),
            callee: RuntimeFn::WfSleep,
            args: vec![Value::Var(Var(0))],
            live_after: LiveAfter::default(),
            span: sp(),
        }],
        Tail::Return(Value::Var(Var(1))),
    );
    let (parsed, _table) = gate(&module("ci", &[], Vec::new(), vec![flow]))?;
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::CallExt { .. })),
        1
    );
    assert_eq!(
        parsed.imports.len(),
        1,
        "one import in first-use order (IR-24)"
    );
    Ok(())
}

/// §11.4 `TryBind` (flattened `result.try`, §2.2: `is_tagged_tuple` + extract,
/// fail to the shared exit).
#[test]
fn shape_try_bind() -> Result<(), Box<dyn std::error::Error>> {
    let flow = flow(
        "execute",
        FnOrigin::Execute,
        &[0],
        vec![
            Stmt::CallRt {
                dst: Some(Var(1)),
                callee: RuntimeFn::MapTimerError,
                args: vec![Value::Var(Var(0))],
                live_after: LiveAfter::default(),
                span: sp(),
            },
            Stmt::TryBind {
                dst: Var(2),
                result: Var(1),
                live_after: LiveAfter::default(),
                span: sp(),
            },
        ],
        Tail::Return(Value::Var(Var(2))),
    );
    let (parsed, _table) = gate(&module("tb", &[], Vec::new(), vec![flow]))?;
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::IsTaggedTuple { .. })),
        1
    );
    Ok(())
}

/// §11.4 frameless `TailImport` (`call_ext_only`, no `Allocate`).
#[test]
fn shape_frameless_tail_import() -> Result<(), Box<dyn std::error::Error>> {
    let flow = flow(
        "execute",
        FnOrigin::Execute,
        &[],
        Vec::new(),
        Tail::TailRt {
            callee: RuntimeFn::DSuccess,
            args: vec![Value::Nil],
        },
    );
    let (parsed, _table) = gate(&module("fl", &[], Vec::new(), vec![flow]))?;
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::Allocate { .. })),
        0,
        "frameless"
    );
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::CallExtOnly { .. })),
        1
    );
    Ok(())
}

/// §11.4 `MakeClosure` (`make_fun2` + `FunT`) of a module-local function.
#[test]
fn shape_make_closure() -> Result<(), Box<dyn std::error::Error>> {
    let host = flow(
        "execute",
        FnOrigin::Execute,
        &[],
        vec![Stmt::MakeClosure {
            dst: Var(0),
            lifted: FnRef(1),
            captures: Vec::new(),
            span: sp(),
        }],
        Tail::Return(Value::Var(Var(0))),
    );
    let target = flow(
        "helper",
        FnOrigin::Region {
            entry_step: "helper".to_owned(),
        },
        &[],
        Vec::new(),
        Tail::Return(Value::Nil),
    );
    let (parsed, _table) = gate(&module("mc", &[], Vec::new(), vec![host, target]))?;
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::MakeFun { .. })),
        1
    );
    assert_eq!(parsed.lambdas.len(), 1, "one FunT entry, first-use");
    Ok(())
}

/// §11.4 `JsonObj` with ≥2 pairs (Y-homed accumulator across the encode calls).
#[test]
fn shape_json_obj_two_pairs() -> Result<(), Box<dyn std::error::Error>> {
    let pairs = vec![
        (
            "a".to_owned(),
            JsonVal::Encoded {
                value: Value::Var(Var(1)),
                via: ToJsonRef::SdkLeaf(Leaf::Str),
            },
        ),
        (
            "b".to_owned(),
            JsonVal::Encoded {
                value: Value::Var(Var(2)),
                via: ToJsonRef::SdkLeaf(Leaf::Str),
            },
        ),
    ];
    let flow = flow(
        "execute",
        FnOrigin::Execute,
        &[0],
        vec![
            Stmt::FieldGet {
                dst: Var(1),
                base: Value::Var(Var(0)),
                index: 1,
                span: sp(),
            },
            Stmt::FieldGet {
                dst: Var(2),
                base: Value::Var(Var(0)),
                index: 2,
                span: sp(),
            },
            Stmt::JsonObj {
                dst: Var(3),
                pairs,
                span: sp(),
            },
        ],
        Tail::Return(Value::Var(Var(3))),
    );
    let (parsed, _table) = gate(&module("jo", &[], Vec::new(), vec![flow]))?;
    // two leaf to_json call_exts + one json:object call_ext.
    assert!(count(&parsed, |i| matches!(i, Instruction::CallExt { .. })) >= 3);
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::PutList { .. })),
        2,
        "two conses"
    );
    Ok(())
}

/// §11.4 `CallLocal` (a `call` into a module-local function's body label).
#[test]
fn shape_call_local() -> Result<(), Box<dyn std::error::Error>> {
    let host = flow(
        "execute",
        FnOrigin::Execute,
        &[],
        vec![Stmt::CallLocal {
            dst: Some(Var(0)),
            callee: FnRef(1),
            args: Vec::new(),
            live_after: LiveAfter::default(),
            span: sp(),
        }],
        Tail::Return(Value::Var(Var(0))),
    );
    let target = flow(
        "helper",
        FnOrigin::Region {
            entry_step: "helper".to_owned(),
        },
        &[],
        Vec::new(),
        Tail::Return(Value::Nil),
    );
    let (parsed, _table) = gate(&module("cl", &[], Vec::new(), vec![host, target]))?;
    assert_eq!(count(&parsed, |i| matches!(i, Instruction::Call { .. })), 1);
    Ok(())
}

/// Determinism at the byte level for a hand-built module, and the literal `Int`
/// argument path (§11.4 `Record` with an inline integer element).
#[test]
fn shape_record_with_literal_int() -> Result<(), Box<dyn std::error::Error>> {
    let flow = flow(
        "execute",
        FnOrigin::Execute,
        &[],
        vec![Stmt::RecordNew {
            dst: Var(0),
            tag: AtomRef(0),
            args: vec![Value::Int(5), Value::Lit(LitRef(0))],
            span: sp(),
        }],
        Tail::Return(Value::Var(Var(0))),
    );
    let module = module("ri", &["pair"], vec![MirLiteral::Integer(7)], vec![flow]);
    let (parsed, _table) = gate(&module)?;
    assert_eq!(
        count(&parsed, |i| matches!(i, Instruction::PutTuple2 { .. })),
        1
    );
    Ok(())
}
