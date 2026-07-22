//! Decoding + per-function analysis support for the BC-5 codegen inspection
//! tests (`inspect_tests`). Everything here reads the beamr `0.15.4` decoder's
//! `Instruction`/`Operand` surface — no writer, no MIR — so the assertions in
//! `inspect_tests` witness the emitted bytes directly (the AWL-BC-IR.md §11 /
//! IR-14 contract), not the emitter's intentions.

use std::fs;
use std::path::PathBuf;

use beamr::atom::{Atom, AtomTable};
use beamr::loader::decode::{BifOp, Instruction, LambdaEntry, Operand, decode_instructions};
use beamr::loader::load::ParsedModule;
use beamr::loader::{load_beam_chunks, parse_beam_chunks};

use crate::mir::{LowerError, MirModule, lower, select};

/// The crate manifest directory (fixtures live beneath it).
pub(super) fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `valid/` rev-2 fixture that the direct compiler lowers to MIR, keyed by
/// a `<dir>/<stem>` label — the covered ratchet the inspection sweeps. Fixtures
/// the compiler refuses (`Unsupported`/`Planning`) are skipped, exactly as the
/// BC-3 emit oracle skips them; a genuine lowering error is surfaced.
///
/// # Errors
///
/// Propagates a parse failure or a non-refusal lowering error.
pub(super) fn lowered_fixtures() -> Result<Vec<(String, MirModule)>, Box<dyn std::error::Error>> {
    let root = manifest_dir().join("tests/fixtures/rev2");
    let mut paths = Vec::new();
    collect_valid(&root, &mut paths);
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let source = fs::read_to_string(&path)?;
        let document = crate::parse(&source)?;
        match lower(&document, path.parent()) {
            Ok(module) => {
                let label = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .with_extension("")
                    .to_string_lossy()
                    .into_owned();
                out.push((label, module));
            }
            Err(LowerError::Unsupported { .. } | LowerError::Planning { .. }) => {}
            Err(other) => return Err(Box::new(other)),
        }
    }
    Ok(out)
}

/// Recursively collects `valid/` `.awl` fixtures beneath `dir`.
fn collect_valid(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_valid(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "awl")
            && path
                .components()
                .any(|component| component.as_os_str() == "valid")
        {
            out.push(path);
        }
    }
}

/// Selects `.beam` bytes for `module` and decodes them into a parsed module and
/// the atom table the decode interned names into.
///
/// # Errors
///
/// Propagates a `select` refusal or a decode failure.
pub(super) fn decode(
    module: &MirModule,
) -> Result<(ParsedModule, AtomTable), Box<dyn std::error::Error>> {
    decode_bytes(&select(module)?)
}

/// Decodes already-selected `.beam` `bytes` into a parsed module and its atom
/// table, WITNESSING that the whole `Code` chunk was decoded — never a silent
/// prefix. Every inspection (sweep and targeted) routes through this one path.
///
/// The beamr Code decoder breaks-and-returns-`Ok` at opcode `int_code_end` (`3`)
/// and discards whatever follows, so a bare `load_beam_chunks` success can cover
/// a decoded PREFIX: an early terminator, a truncated frame, or entire later
/// functions would be invisible while an inspection stayed green (the BC-5 brief
/// decoder-discipline mandate; BC-5 review blocker 2). Two independent witnesses
/// close that hole: the `Code` sub-header's own `function_count` must equal the
/// decoded `FuncInfo` count (an early terminator cannot reduce inspected scope),
/// and the declared instruction stream's last byte must be load-bearing (the
/// BC-4 truncation witness — a trailing terminator never is).
///
/// # Errors
///
/// Propagates a decode failure, or fails when the decoded scope is a prefix of
/// the declared `Code` stream.
pub(super) fn decode_bytes(
    bytes: &[u8],
) -> Result<(ParsedModule, AtomTable), Box<dyn std::error::Error>> {
    let table = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(bytes, &table)?;
    assert_function_count_matches(bytes, &parsed)?;
    assert_code_stream_fully_consumed(bytes, &parsed)?;
    Ok((parsed, table))
}

/// The `Code` chunk body bytes (its 20-byte sub-header plus the declared
/// instruction stream), read at the chunk's DECLARED length — not the padded
/// container length — so no inter-chunk padding rides along.
fn code_chunk_body(bytes: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    parse_beam_chunks(bytes)?
        .iter()
        .find(|(id, _)| id == b"Code")
        .map(|(_, body)| body.to_vec())
        .ok_or_else(|| "no Code chunk".into())
}

/// Reads a big-endian `u32` at `offset`.
fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32, Box<dyn std::error::Error>> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or("Code chunk too short for a u32 read")?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Asserts the `Code` sub-header's declared `function_count` (the `u32` at
/// offset 16) equals the number of `FuncInfo` instructions actually decoded — so
/// an early `int_code_end` cannot silently drop whole later functions from the
/// inspected scope.
fn assert_function_count_matches(
    bytes: &[u8],
    parsed: &ParsedModule,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = code_chunk_body(bytes)?;
    let declared = read_u32_be(&body, 16)?;
    let decoded = u32::try_from(
        parsed
            .instructions
            .iter()
            .filter(|instruction| matches!(instruction, Instruction::FuncInfo { .. }))
            .count(),
    )?;
    if declared != decoded {
        return Err(format!(
            "Code sub-header declares {declared} functions but only {decoded} FuncInfo decoded — \
             the decode covered a prefix (an early int_code_end?)"
        )
        .into());
    }
    Ok(())
}

/// Proves — WITHOUT the module writer — that the declared `Code` instruction
/// stream ends in a genuine, load-bearing instruction byte and so carries no
/// trailing `int_code_end` terminator hiding a truncated tail. Ported from the
/// BC-4 oracle (`crates/aion/tests/awl_bc4_differential/abi.rs`): decode the
/// declared stream, then decode it again with its final byte removed. A trailing
/// terminator (or any byte the decoder drops) is not turned into an instruction,
/// so removing it leaves the decoded sequence UNCHANGED — reject. With no
/// terminator, the final byte belongs to the last real instruction, so removing
/// it changes the sequence — accept.
///
/// # Errors
///
/// Returns an error when the declared stream's last byte is inert (a trailing
/// terminator or opcode), i.e. when the stream is not fully consumed.
fn assert_code_stream_fully_consumed(
    bytes: &[u8],
    parsed: &ParsedModule,
) -> Result<(), Box<dyn std::error::Error>> {
    let code = code_chunk_body(bytes)?;
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
            "the declared Code instruction stream ends in an inert byte (an int_code_end \
             terminator or trailing opcode): dropping its last byte left the decoded sequence \
             unchanged, so no real instruction owns that byte"
                .into(),
        ),
        _ => Ok(()),
    }
}

/// Resolves an external call's `import` operand to its
/// `(module_name, function_name, arity)` through the module's decoded `ImpT`
/// table — the independent target metadata the route/marshaling oracles pin
/// against, rather than trusting the call's own self-declared shape.
pub(super) fn import_target(
    parsed: &ParsedModule,
    table: &AtomTable,
    import: &Operand,
) -> Option<(String, String, u8)> {
    let entry = parsed.imports.get(operand_index(import)?)?;
    Some((
        name_of(table, entry.module),
        name_of(table, entry.function),
        entry.arity,
    ))
}

/// The import-table (or literal-pool) index an operand names, when it is one.
fn operand_index(operand: &Operand) -> Option<usize> {
    match operand {
        Operand::Unsigned(value) => usize::try_from(*value).ok(),
        Operand::Literal(value) => Some(*value),
        _ => None,
    }
}

/// Resolves an atom to its name, or `<?>` when absent.
pub(super) fn name_of(table: &AtomTable, atom: Atom) -> String {
    table.resolve(atom).unwrap_or("<?>").to_owned()
}

/// The physical capture count (`num_free`) of the `FunT` a decoded `make_fun2`
/// names, resolved through the module's `lambdas` table — the implicit inputs
/// `x0..x(num_free-1)` the closure reads (BC-5 review blocker 3). `None` for a
/// non-`MakeFun` instruction.
///
/// # Errors
///
/// Fails when a `MakeFun`'s lambda-index operand is absent or out of range (a
/// decoder/emitter disagreement, surfaced rather than swallowed).
pub(super) fn make_fun_num_free(
    lambdas: &[LambdaEntry],
    instruction: &Instruction,
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let Instruction::MakeFun { operands } = instruction else {
        return Ok(None);
    };
    let index = operands
        .first()
        .and_then(operand_index)
        .ok_or("make_fun2 carries no lambda-index operand")?;
    let entry = lambdas
        .get(index)
        .ok_or_else(|| format!("make_fun2 lambda index {index} out of range in FunT"))?;
    Ok(Some(entry.num_free))
}

/// Rewrites every `make_fun2` in `code` so its implicit capture inputs
/// `x0..x(num_free-1)` become EXPLICIT `X` operands (resolved through `lambdas`),
/// leaving every other instruction untouched. The production selector marshals
/// captures into `x0..` and then emits `MakeFun { operands: [Unsigned(lambda)] }`
/// with the count stored only as the `FunT`'s `num_free` (`select/emit.rs`
/// `MakeClosure`; `builder.rs` `lambda`), so a raw decoded one-operand `make_fun2`
/// contributes ZERO reads to the register-safety/`Live` analyses — a deleted or
/// stale capture register would be invisible. Making the reads explicit here
/// feeds them into the metadata-free CFG ([`super::inspect_cfg`]) uniformly,
/// without threading `lambdas` through every transfer function (BC-5 review
/// blocker 3).
///
/// # Errors
///
/// Propagates a `make_fun2` whose lambda index does not resolve in `lambdas`.
pub(super) fn with_explicit_make_fun_reads(
    code: &[Instruction],
    lambdas: &[LambdaEntry],
) -> Result<Vec<Instruction>, Box<dyn std::error::Error>> {
    let mut out = Vec::with_capacity(code.len());
    for instruction in code {
        if let Instruction::MakeFun { operands } = instruction {
            let num_free = make_fun_num_free(lambdas, instruction)?
                .ok_or("make_fun2 lost its capture count during expansion")?;
            let mut expanded = Vec::with_capacity(operands.len() + num_free as usize);
            expanded.extend((0..num_free).map(Operand::X));
            expanded.extend(operands.iter().cloned());
            out.push(Instruction::MakeFun { operands: expanded });
        } else {
            out.push(instruction.clone());
        }
    }
    Ok(out)
}

/// One decoded function: its name and the contiguous instruction slice from its
/// entry `Label` (the one immediately preceding its `FuncInfo`) up to the next
/// function's entry label (or end of stream).
pub(super) struct DecodedFn<'p> {
    /// The function name (its `FuncInfo` function atom).
    pub(super) name: String,
    /// The full instruction slice, entry label through last instruction.
    pub(super) code: &'p [Instruction],
}

/// Partitions a parsed module's instruction stream into per-function slices.
///
/// A function begins at the `Label` immediately followed by a `FuncInfo`; the
/// slice runs to just before the next such entry label, so it includes the
/// leading `Allocate`, the body, and the shared exit — the whole function.
pub(super) fn functions<'p>(parsed: &'p ParsedModule, table: &AtomTable) -> Vec<DecodedFn<'p>> {
    let stream = &parsed.instructions;
    let mut entries: Vec<(usize, String)> = Vec::new();
    for index in 0..stream.len() {
        if matches!(stream[index], Instruction::Label { .. })
            && let Some(Instruction::FuncInfo { function, .. }) = stream.get(index + 1)
            && let Operand::Atom(Some(atom)) = function
        {
            entries.push((index, name_of(table, *atom)));
        }
    }
    let mut out = Vec::with_capacity(entries.len());
    for position in 0..entries.len() {
        let start = entries[position].0;
        let end = entries
            .get(position + 1)
            .map_or(stream.len(), |(next, _)| *next);
        out.push(DecodedFn {
            name: entries[position].1.clone(),
            code: &stream[start..end],
        });
    }
    out
}

/// The unsigned value of an operand, when it is a plain unsigned count.
pub(super) fn as_unsigned(operand: &Operand) -> Option<u64> {
    match operand {
        Operand::Unsigned(value) => Some(*value),
        _ => None,
    }
}

/// Recursively reports whether an operand mentions any `Y` register.
pub(super) fn operand_has_y(operand: &Operand) -> bool {
    match operand {
        Operand::Y(_) => true,
        Operand::List(items) => items.iter().any(operand_has_y),
        Operand::TypedRegister { register, .. } => operand_has_y(register),
        _ => false,
    }
}

/// Every top-level operand an instruction carries (for a Y-mention scan). List
/// and typed-register nesting is walked by [`operand_has_y`].
pub(super) fn instruction_operands(instruction: &Instruction) -> Vec<&Operand> {
    match instruction {
        Instruction::Move {
            source,
            destination,
        } => vec![source, destination],
        Instruction::Call { arity, label } | Instruction::CallOnly { arity, label } => {
            vec![arity, label]
        }
        Instruction::CallExt { arity, import } | Instruction::CallExtOnly { arity, import } => {
            vec![arity, import]
        }
        Instruction::CallLast {
            arity,
            label,
            deallocate,
        } => vec![arity, label, deallocate],
        Instruction::CallExtLast {
            arity,
            import,
            deallocate,
        } => vec![arity, import, deallocate],
        Instruction::CallFun { arity } => vec![arity],
        Instruction::Allocate { stack_need, live } => vec![stack_need, live],
        Instruction::Deallocate { words } => vec![words],
        Instruction::TestHeap { heap_need, live } => vec![heap_need, live],
        Instruction::PutList {
            head,
            tail,
            destination,
        } => vec![head, tail, destination],
        Instruction::PutTuple2 {
            destination,
            elements,
        } => vec![destination, elements],
        Instruction::GetTupleElement {
            source,
            index,
            destination,
        } => vec![source, index, destination],
        Instruction::GetList { source, head, tail } => vec![source, head, tail],
        Instruction::IsTaggedTuple {
            fail,
            value,
            arity,
            tag,
        } => vec![fail, value, arity, tag],
        Instruction::Comparison {
            fail, left, right, ..
        } => vec![fail, left, right],
        Instruction::TypeTest { fail, value, .. } => vec![fail, value],
        Instruction::SelectVal { value, fail, list } => vec![value, fail, list],
        Instruction::Jump { target } => vec![target],
        Instruction::Bif { operands, .. } | Instruction::MakeFun { operands } => {
            operands.iter().collect()
        }
        Instruction::Badmatch { value } | Instruction::CaseEnd { value } => vec![value],
        Instruction::FuncInfo {
            module,
            function,
            arity,
        } => vec![module, function, arity],
        _ => Vec::new(),
    }
}

/// The `X` register index of an operand, if it names one directly.
fn x_index(operand: &Operand) -> Option<u32> {
    match operand {
        Operand::X(index) => Some(*index),
        Operand::TypedRegister { register, .. } => x_index(register),
        _ => None,
    }
}

/// Collects every `X` index appearing anywhere in an operand (walking lists).
fn collect_x(operand: &Operand, into: &mut Vec<u32>) {
    match operand {
        Operand::X(index) => into.push(*index),
        Operand::List(items) => {
            for item in items {
                collect_x(item, into);
            }
        }
        Operand::TypedRegister { register, .. } => collect_x(register, into),
        _ => {}
    }
}

/// Pushes an operand's direct `X` index into `into`, when it names one.
fn push_x(operand: &Operand, into: &mut Vec<u32>) {
    if let Some(index) = x_index(operand) {
        into.push(index);
    }
}

/// The `X` registers an instruction reads (a call reads its argument registers,
/// `call_fun` also the fun in `x(arity)`; a heap/put/test op reads its element
/// or subject registers; a `Move` reads its source; a `Return` reads `x0`, the
/// value it hands back by ABI — BC-5 review blocker 4).
fn x_reads(instruction: &Instruction) -> Vec<u32> {
    let mut reads = Vec::new();
    match instruction {
        // A `Return` consumes the ABI result register `x0`. Modeling this
        // implicit read keeps the backward-liveness `Live` and the forward
        // must-define analyses from accepting a `Return` of a cleared/undefined
        // `x0` (BC-5 review blocker 4).
        Instruction::Return => reads.push(0),
        Instruction::Move { source, .. } => push_x(source, &mut reads),
        Instruction::Call { arity, .. }
        | Instruction::CallOnly { arity, .. }
        | Instruction::CallExt { arity, .. }
        | Instruction::CallExtOnly { arity, .. }
        | Instruction::CallLast { arity, .. }
        | Instruction::CallExtLast { arity, .. } => {
            if let Some(count) = as_unsigned(arity) {
                reads.extend(0..u32::try_from(count).unwrap_or(0));
            }
        }
        Instruction::CallFun { arity } => {
            if let Some(count) = as_unsigned(arity) {
                reads.extend(0..=u32::try_from(count).unwrap_or(0));
            }
        }
        Instruction::Bif { operands, .. } => {
            let mut xs = Vec::new();
            for operand in operands {
                collect_x(operand, &mut xs);
            }
            if let Some((_last, rest)) = xs.split_last() {
                reads.extend_from_slice(rest);
            }
        }
        // A `make_fun` reads every captured `X` register (its environment); its
        // result lands in `x0` (a write, not a read). The raw decoded op carries
        // only the lambda index; the capture reads `x0..x(num_free-1)` are made
        // EXPLICIT by [`with_explicit_make_fun_reads`] (resolved through the
        // module's `FunT`) before analysis, so `collect_x` sees them here and the
        // cross-call X-safety / `Live` analyses stay complete (BC-5 review
        // blocker 3).
        Instruction::MakeFun { operands } => {
            for operand in operands {
                collect_x(operand, &mut reads);
            }
        }
        Instruction::PutTuple2 { elements, .. } => collect_x(elements, &mut reads),
        Instruction::PutList { head, tail, .. } => {
            push_x(head, &mut reads);
            push_x(tail, &mut reads);
        }
        Instruction::GetTupleElement { source, .. } | Instruction::GetList { source, .. } => {
            push_x(source, &mut reads);
        }
        Instruction::IsTaggedTuple { value, .. }
        | Instruction::TypeTest { value, .. }
        | Instruction::SelectVal { value, .. }
        | Instruction::Badmatch { value }
        | Instruction::CaseEnd { value } => push_x(value, &mut reads),
        Instruction::Comparison { left, right, .. } => {
            push_x(left, &mut reads);
            push_x(right, &mut reads);
        }
        _ => {}
    }
    reads
}

/// The `X` registers an instruction writes (a call/`make_fun` leaves its result
/// in `x0`; a heap/put/get op writes its destination register(s); a `Move`
/// writes its destination).
fn x_writes_all(instruction: &Instruction) -> Vec<u32> {
    let mut writes = Vec::new();
    match instruction {
        Instruction::Move { destination, .. }
        | Instruction::GetTupleElement { destination, .. }
        | Instruction::PutTuple2 { destination, .. }
        | Instruction::PutList { destination, .. } => push_x(destination, &mut writes),
        Instruction::Call { .. }
        | Instruction::CallOnly { .. }
        | Instruction::CallExt { .. }
        | Instruction::CallExtOnly { .. }
        | Instruction::CallLast { .. }
        | Instruction::CallExtLast { .. }
        | Instruction::CallFun { .. }
        | Instruction::MakeFun { .. } => writes.push(0),
        Instruction::GetList { head, tail, .. } => {
            push_x(head, &mut writes);
            push_x(tail, &mut writes);
        }
        Instruction::Bif { operands, .. } => {
            let mut xs = Vec::new();
            for operand in operands {
                collect_x(operand, &mut xs);
            }
            if let Some(last) = xs.last() {
                writes.push(*last);
            }
        }
        _ => {}
    }
    writes
}

/// The `X` registers an instruction reads and the `X` registers it writes.
///
/// Grounded in the BC-3 emitter's closed instruction alphabet (§11.4): a call
/// reads its argument registers and leaves its result in `x0`; a heap/put op
/// reads its element registers and writes its destination; a `Move` reads its
/// source and writes its destination.
pub(super) fn reads_writes(instruction: &Instruction) -> (Vec<u32>, Vec<u32>) {
    (x_reads(instruction), x_writes_all(instruction))
}

/// The destination (write-target) operands an instruction carries. Used to
/// witness that a `Y` register is only ever WRITTEN by a `move` (spills, stores,
/// `TryBind`, json accumulators, `let assert` binds), even though several
/// non-call test/heap ops READ a `Y` home directly (the §11.2 divergence).
pub(super) fn destinations(instruction: &Instruction) -> Vec<&Operand> {
    match instruction {
        Instruction::Move { destination, .. }
        | Instruction::GetTupleElement { destination, .. }
        | Instruction::PutList { destination, .. }
        | Instruction::PutTuple2 { destination, .. } => vec![destination],
        Instruction::GetList { head, tail, .. } => vec![head, tail],
        Instruction::Bif { operands, .. } => operands.last().into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Whether an instruction is a call (an X-clobbering transfer): after it, `x0`
/// holds the result and every higher `X` is dead.
pub(super) fn is_call(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::Call { .. }
            | Instruction::CallOnly { .. }
            | Instruction::CallExt { .. }
            | Instruction::CallExtOnly { .. }
            | Instruction::CallLast { .. }
            | Instruction::CallExtLast { .. }
            | Instruction::CallFun { .. }
            | Instruction::MakeFun { .. }
    )
}

/// The `Live` operand a `TestHeap` or `GcBif` heap op declares, if this is one.
/// `GcBif` carries `Live` as its second operand; `TestHeap` as its named field.
pub(super) fn heap_live(instruction: &Instruction) -> Option<u32> {
    match instruction {
        Instruction::TestHeap { live, .. } => as_unsigned(live).and_then(|v| u32::try_from(v).ok()),
        Instruction::Bif { op, operands } if is_gc_bif(*op) => operands
            .get(1)
            .and_then(as_unsigned)
            .and_then(|v| u32::try_from(v).ok()),
        _ => None,
    }
}

/// Whether a `BifOp` is a garbage-collecting BIF (a GC point carrying `Live`).
fn is_gc_bif(op: BifOp) -> bool {
    matches!(op, BifOp::GcBif1 | BifOp::GcBif2 | BifOp::GcBif3)
}
