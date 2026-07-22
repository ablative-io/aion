//! Deliverable 3: the ABI contract tests (`AWL-BC-IR.md` §7), asserted against
//! the RELOADED SPLICED ENTRY MODULE the engine actually loads — not fresh
//! `select()` output (BLOCKER 4).
//!
//! - IR-15 export set: exactly `definition/0`, `run/1`, `execute/1`; no
//!   `module_info/0,1`.
//! - IR-13 entry ABI: `run/1` is exported at arity one (the raw input payload
//!   term), and — wired through a real run — a completing fixture records its
//!   `{ok, ResultBinary}` bytes verbatim as `WorkflowCompleted.result`, while an
//!   error-path fixture records `WorkflowFailed` (the `{error, AwlErrorTerm}`
//!   path). The deeper x/y-register calling convention (IR-14) is not asserted
//!   here — it is exercised end-to-end by every executing fixture in the
//!   differential (`covered.rs`); no test is named for a contract it does not
//!   check.
//! - IR-12 module mangling: the entry module's own atom equals the reference
//!   mangled name AND its `ImpT` carries mangled IMPORTED module references
//!   (`aion@…`).
//! - IR-2 float literal byte-parity: a `Float` literal in `LitT` equals Rust's
//!   parse of the same source lexeme.
//! - Writer contract: canonical chunk order, `AtU8`+`Code` always present, and
//!   every optional chunk present IFF its decoded table is non-empty (the
//!   header-count/nonempty contract), with no foreign chunk. beamr's decoder
//!   stops at end-of-bytes and models no `int_code_end`, so the "no terminator"
//!   contract is inherent in a total decode rather than a byte-hunt.
//!
//! Rows pinned inside `aion-awl` are referenced, not duplicated: IR-5/6/7/8
//! (Option/Result/record/enum reps) execute under
//! `aion-awl/tests/runtime_codecs.rs`; IR-10 SDK constructor atoms at
//! `src/mir/lower/stmts.rs`; the export set is built at
//! `src/mir/select/assemble.rs`.

use beamr::atom::AtomTable;
use beamr::loader::decode::Literal;
use beamr::loader::load::ParsedModule;
use beamr::loader::{load_beam_chunks, parse_beam_chunks};

use aion_core::Event;

use crate::driver::{SplicedFixture, build_spliced};
use crate::run::{Disposition, run_package};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The canonical writer chunk order (`encode/container.rs`). Optional chunks
/// (everything after `Code`) appear only when non-empty, but always in this
/// relative order.
const CANONICAL_CHUNKS: &[&[u8; 4]] = &[
    b"AtU8", b"Code", b"ImpT", b"ExpT", b"FunT", b"LitT", b"StrT", b"Line",
];

/// The ABI fixtures: the flagship (a completing activity workflow with imports)
/// and a float-guarded fixture (a `Float` literal + a data-driven error path).
fn abi_fixtures() -> Vec<String> {
    vec![
        String::from("flagship/valid/awl_hello"),
        String::from("loop-outcomes/valid/float_threshold_guard"),
    ]
}

/// Decodes bytes into `(atom table, parsed module)` so export/atom names
/// resolve against the same table the decode interned them into.
fn decode(bytes: &[u8]) -> Result<(AtomTable, ParsedModule), Box<dyn std::error::Error>> {
    let table = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(bytes, &table)?;
    Ok((table, parsed))
}

/// The spliced entry bytes for a fixture (the bytes the engine loads).
fn entry_bytes<'a>(fixtures: &'a [SplicedFixture], entry_module: &str) -> Option<&'a [u8]> {
    fixtures
        .iter()
        .find(|fixture| fixture.entry_module == entry_module)
        .map(|fixture| fixture.entry_bytes.as_slice())
}

/// Every static IR row, asserted against the RELOADED SPLICED entry beam.
#[tokio::test(flavor = "multi_thread")]
async fn abi_static_rows_over_spliced_entry() -> TestResult {
    let fixtures = build_spliced(&abi_fixtures(), "abi_static").await?;

    // The spliced entry bytes MUST be exactly the direct beam the package
    // carries (BLOCKER 4: assert against loaded bytes, not fresh select()).
    for fixture in &fixtures {
        let loaded = fixture
            .direct_package
            .beams()
            .get(&fixture.entry_module)
            .ok_or("spliced package lost its entry beam")?;
        assert_eq!(
            loaded, fixture.entry_bytes,
            "the ABI decode target must be the loaded spliced entry"
        );
    }

    let hello = entry_bytes(&fixtures, "awl_hello").ok_or("no awl_hello entry")?;
    let (table, parsed) = decode(hello)?;

    // IR-15: exactly definition/0, run/1, execute/1; no module_info.
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

    // IR-13 (static half): run/1 exported at arity one.
    assert!(
        parsed
            .exports
            .iter()
            .any(|export| table.resolve(export.function) == Some("run") && export.arity == 1),
        "IR-13: run/1 must be exported at arity one"
    );

    // IR-12: own module atom equals the reference mangled name, AND ImpT
    // carries mangled aion@… imported module references.
    assert_eq!(
        table.resolve(parsed.name),
        Some("awl_hello"),
        "IR-12: entry module atom drifted"
    );
    let imported: Vec<String> = parsed
        .imports
        .iter()
        .filter_map(|import| table.resolve(import.module))
        .map(str::to_owned)
        .collect();
    assert!(
        imported.iter().any(|module| module.starts_with("aion@")),
        "IR-12: ImpT must carry mangled aion@ imported modules, got {imported:?}"
    );

    // Writer contract, over the spliced entry.
    assert_writer_contract(hello)?;

    // IR-2: the 0.5 float literal appears in float_threshold_guard's LitT.
    let guard = entry_bytes(&fixtures, "float_threshold_guard").ok_or("no guard entry")?;
    let (_guard_table, guard_parsed) = decode(guard)?;
    let expected: f64 = "0.5".parse()?;
    let mut floats = Vec::new();
    for literal in &guard_parsed.literals {
        collect_floats(literal, &mut floats);
    }
    assert!(
        floats
            .iter()
            .any(|value| value.to_bits() == expected.to_bits()),
        "IR-2: the 0.5 float literal must appear byte-identically in LitT, found {floats:?}"
    );
    assert_writer_contract(guard)?;
    Ok(())
}

/// IR-13, wired through a real run of the SPLICED direct package: a completing
/// fixture records `{ok, ResultBinary}` bytes verbatim as
/// `WorkflowCompleted.result`, and an error-path fixture records
/// `WorkflowFailed` (the `{error, AwlErrorTerm}` path).
#[tokio::test(flavor = "multi_thread")]
async fn abi_entry_result_recording_through_the_trail() -> TestResult {
    let fixtures = build_spliced(&abi_fixtures(), "abi_run").await?;

    let hello = fixtures
        .iter()
        .find(|fixture| fixture.entry_module == "awl_hello")
        .ok_or("no awl_hello")?;
    let outcome = run_package(
        hello.direct_package.clone(),
        "awl_hello",
        &hello.input,
        hello.action_results.clone(),
    )
    .await?;
    assert_eq!(
        outcome.disposition,
        Disposition::Completed,
        "IR-13: awl_hello must complete its real greet -> shout path"
    );
    let result = completed_result(&outcome.trail).ok_or("no WorkflowCompleted.result recorded")?;
    assert_eq!(
        result,
        br#"{"outcome":"shouted","payload":{"text":"x"}}"#.to_vec(),
        "IR-13: the ResultBinary recorded on the trail drifted: {}",
        String::from_utf8_lossy(&result)
    );

    let guard = fixtures
        .iter()
        .find(|fixture| fixture.entry_module == "float_threshold_guard")
        .ok_or("no float_threshold_guard")?;
    let outcome = run_package(
        guard.direct_package.clone(),
        "float_threshold_guard",
        &guard.input,
        guard.action_results.clone(),
    )
    .await?;
    assert_eq!(
        outcome.disposition,
        Disposition::Failed,
        "IR-13: float_threshold_guard's 0.0 < 0.5 guard must take the error path"
    );
    assert!(
        outcome
            .trail
            .iter()
            .any(|event| matches!(event, Event::WorkflowFailed { .. })),
        "IR-13: the error path must record WorkflowFailed (the error-term path)"
    );
    Ok(())
}

/// The recorded `WorkflowCompleted.result` payload bytes, if any.
fn completed_result(trail: &[Event]) -> Option<Vec<u8>> {
    trail.iter().find_map(|event| match event {
        Event::WorkflowCompleted { result, .. } => Some(result.bytes().to_vec()),
        _ => None,
    })
}

/// Asserts the writer chunk contract over `bytes`: canonical order, `AtU8` and
/// `Code` always present, every optional chunk present IFF its decoded table is
/// non-empty, and no foreign chunk.
fn assert_writer_contract(bytes: &[u8]) -> TestResult {
    let chunks = parse_beam_chunks(bytes)?;
    let names: Vec<[u8; 4]> = chunks.iter().map(|(id, _)| *id).collect();
    assert!(
        names.first() == Some(b"AtU8") && names.get(1) == Some(b"Code"),
        "AtU8 then Code must lead the container, got {:?}",
        render_chunks(&names)
    );
    for id in &names {
        assert!(
            CANONICAL_CHUNKS.contains(&id),
            "foreign chunk {:?} in the container",
            String::from_utf8_lossy(id)
        );
    }
    assert_eq!(
        names,
        canonical_subsequence(&names),
        "chunks out of canonical order: {:?}",
        render_chunks(&names)
    );
    // Optional chunk present IFF its decoded table is non-empty.
    let (_table, parsed) = decode(bytes)?;
    let present = |id: &[u8; 4]| names.iter().any(|name| name == id);
    assert_eq!(
        present(b"ImpT"),
        !parsed.imports.is_empty(),
        "ImpT vs imports"
    );
    assert_eq!(
        present(b"ExpT"),
        !parsed.exports.is_empty(),
        "ExpT vs exports"
    );
    assert_eq!(
        present(b"FunT"),
        !parsed.lambdas.is_empty(),
        "FunT vs lambdas"
    );
    assert_eq!(
        present(b"LitT"),
        !parsed.literals.is_empty(),
        "LitT vs literals"
    );
    assert_eq!(
        present(b"StrT"),
        !parsed.string_table.is_empty(),
        "StrT vs string table"
    );
    assert_eq!(
        present(b"Line"),
        !parsed.line_info.is_empty(),
        "Line vs line info"
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
