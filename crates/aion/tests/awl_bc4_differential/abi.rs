//! Deliverable 3: the ABI contract tests (`AWL-BC-IR.md` §7), asserted against
//! the RELOADED SPLICED ENTRY MODULE the engine actually loads — not fresh
//! `select()` output (BLOCKER 4).
//!
//! - IR-15 export set: exactly `definition/0`, `run/1`, `execute/1`; no
//!   `module_info/0,1`.
//! - IR-13 entry ABI: `run/1` is exported at arity one (the raw input payload
//!   term), and — wired through a real run — a completing fixture records its
//!   `{ok, ResultBinary}` bytes verbatim as `WorkflowCompleted.result`, while an
//!   error-path fixture records `WorkflowFailed` whose decoded error term is an
//!   `AwlError`-family tag (the `{error, AwlErrorTerm}` path).
//! - IR-14 calling convention (x/y registers, tail calls): NOT proven here.
//!   Per the coordinator's ruling (recorded in `BC-4-DIFFERENTIAL-BRIEF.md`),
//!   register allocation and tail-call instruction shape are not observable at
//!   the trail level; they are exercised structurally by aion-awl's `select`
//!   tests, and direct byte/instruction assertion is deferred to BC-5 codegen
//!   inspection. No test here claims IR-14.
//! - IR-12 module mangling: the entry module's own atom equals the reference
//!   mangled name AND EVERY imported module in `ImpT` is correctly mangled
//!   (exact expected-set equality, not "any starts with `aion@`").
//! - IR-2 float literal byte-parity: a `Float` literal in `LitT` equals Rust's
//!   parse of the same source lexeme.
//! - Writer contract: canonical chunk order, `AtU8`+`Code` always present, and
//!   every optional chunk present IFF its decoded table is non-empty (the
//!   header-count/nonempty contract), with no foreign chunk. The "no
//!   `int_code_end` terminator" row is NOT witnessed by a successful decode:
//!   beamr's decoder reads opcodes until it SEES opcode `3`, then breaks and
//!   returns `Ok`, silently dropping that terminator and anything after it. It is
//!   proven writer-independently by `assert_code_stream_fully_consumed` — the
//!   declared `Code` stream's last byte must be load-bearing (removing it must
//!   change the decoded instructions), which a trailing terminator never is.
//!
//! Rows pinned inside `aion-awl` are referenced, not duplicated: IR-5/6/7/8
//! (Option/Result/record/enum reps) execute under
//! `aion-awl/tests/runtime_codecs.rs`; IR-10 SDK constructor atoms at
//! `src/mir/lower/stmts.rs`; the export set is built at
//! `src/mir/select/assemble.rs`.

use beamr::atom::AtomTable;
use beamr::loader::decode::{Instruction, Literal, decode_instructions};
use beamr::loader::encode::encode_module;
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

/// The exact distinct set of mangled modules `awl_hello`'s entry imports (IR-12).
const EXPECTED_HELLO_IMPORTS: &[&str] = &[
    "aion@activity",
    "aion@awl@codec",
    "aion@awl@error",
    "aion@awl@runtime",
    "aion@codec",
    "aion@error",
    "aion@workflow",
    "gleam@dynamic@decode",
    "gleam@json",
];

/// The `AwlError` tag-atom family (IR-10) — the closed set of error terms a
/// generated `run/1` may return in `{error, AwlErrorTerm}`.
const AWL_ERROR_TAGS: &[&str] = &[
    "AwlActivityFailed",
    "AwlChildFailed",
    "AwlDecodeInputFailed",
    "AwlError",
    "AwlFailed",
    "AwlIndexOutOfRange",
    "AwlOutcomeFailure",
    "AwlSignalFailed",
    "AwlTimedOut",
    "AwlTimerFailed",
    "AwlVisitsExceeded",
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
    // IR-12: EVERY imported module atom is correctly mangled — the distinct
    // ImpT module set equals the exact expected set (not "any starts with
    // aion@", which one intact import could satisfy while others drift).
    let mut imported: Vec<String> = parsed
        .imports
        .iter()
        .filter_map(|import| table.resolve(import.module))
        .map(str::to_owned)
        .collect();
    imported.sort();
    imported.dedup();
    let mut expected_imports: Vec<String> = EXPECTED_HELLO_IMPORTS
        .iter()
        .map(|module| (*module).to_owned())
        .collect();
    expected_imports.sort();
    assert_eq!(
        imported, expected_imports,
        "IR-12: awl_hello's ImpT module set drifted"
    );
    for module in &imported {
        assert!(
            module.starts_with("aion@") || module.starts_with("gleam@"),
            "IR-12: imported module `{module}` is not mangled (aion@/gleam@)"
        );
    }

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
        &hello.entry_bytes,
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
        &guard.entry_bytes,
    )
    .await?;
    assert_eq!(
        outcome.disposition,
        Disposition::Failed,
        "IR-13: float_threshold_guard's 0.0 < 0.5 guard must take the error path"
    );
    // Error half: decode WorkflowFailed.error.details and assert the term is an
    // AwlError-family tag (IR-10 set) — not merely "some WorkflowFailed". A
    // route-failure lowers to `{error, AwlOutcomeFailure{…}}`.
    let details =
        failed_details(&outcome.trail).ok_or("no WorkflowFailed.error.details recorded")?;
    let tag = details
        .get("tag")
        .and_then(serde_json::Value::as_str)
        .ok_or("WorkflowFailed error term has no tag")?;
    assert!(
        AWL_ERROR_TAGS.contains(&tag),
        "IR-13: the error term tag `{tag}` is not in the AwlError family {AWL_ERROR_TAGS:?}"
    );
    assert_eq!(
        tag, "AwlOutcomeFailure",
        "IR-13: a route-failure must produce the AwlOutcomeFailure term, got `{tag}`"
    );
    Ok(())
}

/// Mutation test for the no-`int_code_end` writer row: injecting the forbidden
/// terminator (opcode 3) into an otherwise-valid Code stream must be REJECTED by
/// the writer contract, even though the mutated module still decodes cleanly
/// (beamr's decoder stops at opcode 3). This is the executable acceptance test
/// for `assert_code_stream_fully_consumed`.
#[tokio::test(flavor = "multi_thread")]
async fn int_code_end_terminator_is_rejected() -> TestResult {
    let fixtures = build_spliced(&abi_fixtures(), "abi_mutation").await?;
    let hello = entry_bytes(&fixtures, "awl_hello").ok_or("no awl_hello entry")?;

    // The clean stream is fully consumed.
    assert_code_stream_fully_consumed(hello)?;

    // Inject `int_code_end` and confirm the module STILL decodes (so it is the
    // fully-consumed check, not the decoder, that catches the terminator)...
    let mutated = inject_int_code_end(hello)?;
    let table = AtomTable::with_common_atoms();
    assert!(
        load_beam_chunks(&mutated, &table).is_ok(),
        "the mutated module must still decode (the decoder stops at opcode 3) — \
         otherwise the mutation would be caught trivially, not by the row's check"
    );

    // ...and the consumption oracle MUST reject it REGARDLESS of how it was
    // produced. `inject_int_code_end` writes exactly the bytes a regressed writer
    // would emit — append opcode 3, bump the declared Code length by one, consume
    // a padding byte — and the oracle reads only the decoder and the declared
    // length, never `encode_module`. So it cannot distinguish, and rejects alike,
    // an externally injected terminator and a writer-originated one. This single
    // case therefore covers both provenances.
    let refusal = assert_code_stream_fully_consumed(&mutated)
        .err()
        .ok_or("int_code_end terminator was NOT rejected — the no-terminator row is vacuous")?;
    // Pin the rejection to the truncation witness ITSELF (its distinctive
    // `inert byte` error). Under the CURRENT writer, a reverted circular
    // re-encode oracle also rejects this injection (the re-encode omits the
    // terminator, so the bytes differ) — while silently missing a
    // writer-originated terminator. Only the error identity separates the two
    // implementations, so a revert to the circular oracle fails HERE.
    assert!(
        refusal.to_string().contains("inert byte"),
        "the rejection must come from the writer-independent truncation witness \
         (its `inert byte` error), not the round-trip fidelity check: {refusal}"
    );
    // The writer contract calls that oracle, so it rejects the terminator too.
    assert!(
        assert_writer_contract(&mutated).is_err(),
        "the writer contract must reject an int_code_end terminator"
    );
    Ok(())
}

/// Appends an `int_code_end` (opcode 3) byte to a module's Code instruction
/// stream, patching the Code chunk length. The Code body length is bumped by
/// one, overwriting the first 4-byte-alignment padding byte, so the container's
/// total size is unchanged (`padded(n) == padded(n + 1)` when `n % 4 != 0`).
fn inject_int_code_end(bytes: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut out = bytes.to_vec();
    let mut offset = 12; // past "FOR1", form size, "BEAM"
    while offset + 8 <= out.len() {
        let length = read_u32_be(&out, offset + 4)? as usize;
        if &out[offset..offset + 4] == b"Code" {
            let padded = length.div_ceil(4) * 4;
            if padded == length {
                return Err("Code body is 4-aligned; no padding byte to inject into".into());
            }
            let terminator_at = offset + 8 + length;
            *out.get_mut(terminator_at)
                .ok_or("Code chunk truncated before its padding")? = 3;
            let new_length = u32::try_from(length + 1)?.to_be_bytes();
            out[offset + 4..offset + 8].copy_from_slice(&new_length);
            return Ok(out);
        }
        offset += 8 + length.div_ceil(4) * 4;
    }
    Err("no Code chunk to mutate".into())
}

/// The recorded `WorkflowCompleted.result` payload bytes, if any.
fn completed_result(trail: &[Event]) -> Option<Vec<u8>> {
    trail.iter().find_map(|event| match event {
        Event::WorkflowCompleted { result, .. } => Some(result.bytes().to_vec()),
        _ => None,
    })
}

/// The decoded `WorkflowFailed.error.details` JSON, if present.
fn failed_details(trail: &[Event]) -> Option<serde_json::Value> {
    trail.iter().find_map(|event| match event {
        Event::WorkflowFailed { error, .. } => error
            .details
            .as_ref()
            .and_then(|payload| serde_json::from_slice(payload.bytes()).ok()),
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

    // Code header counts derived from the instruction stream, never hand-set
    // (AWL-BC-IR.md writer-contract "header counts" row). The Code sub-header is
    // `sub_size(0), version(4), max_opcode(8), label_count(12), function_count(16)`.
    let code = chunks
        .iter()
        .find(|(id, _)| id == b"Code")
        .map(|(_, body)| *body)
        .ok_or("no Code chunk")?;
    assert_eq!(read_u32_be(code, 0)?, 16, "Code sub_size must be 16");
    let label_count = read_u32_be(code, 12)?;
    let function_count = read_u32_be(code, 16)?;
    let func_infos = u32::try_from(
        parsed
            .instructions
            .iter()
            .filter(|instruction| matches!(instruction, Instruction::FuncInfo { .. }))
            .count(),
    )?;
    let max_label = parsed
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::Label { label } => Some(*label),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    assert_eq!(
        function_count, func_infos,
        "Code function_count must equal the FuncInfo count (derived from stream)"
    );
    assert_eq!(
        label_count, max_label,
        "Code label_count must equal the highest Label in the stream (derived, never hand-set)"
    );
    // Round-trip fidelity (a real property, NOT the no-terminator evidence):
    // decode -> encode -> the Code body reproduces byte-for-byte, so for this
    // module the writer is an exact inverse of the decoder.
    assert_code_roundtrip_fidelity(bytes)?;
    // No `int_code_end` terminator (writer-contract row, `AWL-BC-IR.md`). Proven
    // WITHOUT the writer: a successful decode cannot witness the terminator's
    // absence (the decoder breaks at opcode 3 and drops it), so require instead
    // the declared Code stream's last byte to be load-bearing.
    assert_code_stream_fully_consumed(bytes)?;
    Ok(())
}

/// Proves — WITHOUT the module writer — that the declared `Code` instruction
/// stream ends in a genuine, load-bearing instruction byte: it carries no
/// trailing `int_code_end` terminator (opcode `3`) nor any other inert tail byte
/// for a forbidden opcode to hide in.
///
/// # Why a successful decode is not enough
///
/// `int_code_end` is opcode `3`. beamr's Code decoder (`decode/code.rs`) reads
/// opcodes until it SEES opcode `3`, then breaks and returns `Ok`, NEVER
/// materialising the terminator as an `Instruction` and silently discarding
/// whatever follows. So `load_beam_chunks` succeeding says nothing about a
/// terminator's presence.
///
/// # Why this oracle cannot be fooled by a self-consistent writer regression
///
/// It never calls `encode_module` (the writer under test). Its only inputs are
/// beamr's DECODER and the `Code` chunk's own declared length.
/// `parse_beam_chunks` returns each chunk body at its DECLARED length, not the
/// 4-byte-padded container length (`parser.rs`), so `body[20..]` is exactly the
/// declared instruction stream with no padding riding along — the
/// declared-vs-padded distinction the `inject_int_code_end` helper exploits.
///
/// The witness is whether the stream's LAST byte is load-bearing. We decode the
/// declared stream, then decode it again with its final byte removed:
///
/// * A trailing terminator (or any byte the decoder drops) is not turned into an
///   instruction, so removing it leaves the decoded sequence UNCHANGED — the byte
///   was inert, so reject.
/// * With no terminator, the final byte belongs to the last real instruction, so
///   removing it truncates that instruction's operand read (a decode error) or
///   deletes a zero-operand final opcode; either way the sequence CHANGES — the
///   declared stream is fully consumed by instructions, so accept.
///
/// This defeats the padding-exploiting mutation that a naive "the decoder's read
/// cursor reached the declared end" position probe cannot: that mutation parks
/// the terminator as the very LAST declared byte, so the decoder still advances
/// its cursor to the end (reading, then dropping, the terminator) and a position
/// probe reports full consumption. This oracle asks whether the last byte MEANT
/// anything, not merely whether it was read — and a terminator never does.
///
/// Kills two mutations: an externally injected terminator AND a writer-originated
/// one. `inject_int_code_end` produces bytes byte-identical to what a regressed
/// writer would emit, so rejecting them rejects the writer-originated case too —
/// the oracle cannot tell, or care, how the terminator got there.
///
/// # Errors
///
/// Returns an error when the declared stream's last byte is inert (a trailing
/// terminator or opcode), i.e. when the stream is not fully consumed.
fn assert_code_stream_fully_consumed(bytes: &[u8]) -> TestResult {
    let (_table, parsed) = decode(bytes)?;
    let code = code_chunk_body(bytes)?;
    // `body[20..]` is the declared instruction stream, past the 20-byte Code
    // sub-header (five `u32` fields at offsets 0..20; the `sub_size` field's value
    // of 16 counts the four fields that follow it).
    let stream = code
        .get(20..)
        .ok_or("Code chunk shorter than its 20-byte sub-header")?;
    let last = stream
        .len()
        .checked_sub(1)
        .ok_or("empty Code instruction stream")?;
    let (full, _) = decode_instructions(stream, &parsed.atoms, &parsed.literals)?;
    match decode_instructions(&stream[..last], &parsed.atoms, &parsed.literals) {
        Ok((truncated, _)) if truncated == full => Err(
            "the declared Code instruction stream ends in an inert byte (an \
             int_code_end terminator or trailing opcode): dropping its last byte \
             left the decoded instruction sequence unchanged, so no real \
             instruction owns that byte"
                .into(),
        ),
        _ => Ok(()),
    }
}

/// Round-trip fidelity check (a real property, NOT the no-terminator evidence):
/// the decoded module, re-encoded by beamr's own `encode_module`, must reproduce
/// the `Code` chunk body BYTE-FOR-BYTE — so for this module the writer is an
/// exact inverse of the decoder. The no-terminator row is proven separately and
/// writer-independently by `assert_code_stream_fully_consumed`; this assertion
/// must NOT be read as evidence for it, since a writer that emitted a terminator
/// would reproduce it here, self-consistently.
///
/// # Errors
///
/// Returns an error when the re-encoded `Code` body differs from the original.
fn assert_code_roundtrip_fidelity(bytes: &[u8]) -> TestResult {
    let (table, parsed) = decode(bytes)?;
    let reencoded = encode_module(&parsed, &table)?;
    let original_code = code_chunk_body(bytes)?;
    let reencoded_code = code_chunk_body(&reencoded)?;
    if original_code != reencoded_code {
        return Err(format!(
            "the re-encoded Code body differs from the original ({} vs {} bytes)",
            original_code.len(),
            reencoded_code.len()
        )
        .into());
    }
    Ok(())
}

/// The Code chunk body bytes (16-byte sub-header + instruction stream).
fn code_chunk_body(bytes: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    parse_beam_chunks(bytes)?
        .iter()
        .find(|(id, _)| id == b"Code")
        .map(|(_, body)| body.to_vec())
        .ok_or_else(|| "no Code chunk".into())
}

/// Reads a big-endian `u32` at `offset` from `bytes`.
fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32, Box<dyn std::error::Error>> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or("chunk too short for u32 read")?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
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
