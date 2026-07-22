//! Decoding + per-function analysis support for the BC-5 codegen inspection
//! tests (`inspect_tests`). Everything here reads the beamr `0.15.4` decoder's
//! `Instruction`/`Operand` surface — no writer, no MIR — so the assertions in
//! `inspect_tests` witness the emitted bytes directly (the AWL-BC-IR.md §11 /
//! IR-14 contract), not the emitter's intentions.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use beamr::atom::{Atom, AtomTable};
use beamr::loader::decode::{BifOp, Instruction, Operand};
use beamr::loader::load::ParsedModule;
use beamr::loader::load_beam_chunks;

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
    let bytes = select(module)?;
    let table = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&bytes, &table)?;
    Ok((parsed, table))
}

/// Resolves an atom to its name, or `<?>` when absent.
pub(super) fn name_of(table: &AtomTable, atom: Atom) -> String {
    table.resolve(atom).unwrap_or("<?>").to_owned()
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
/// or subject registers; a `Move` reads its source).
fn x_reads(instruction: &Instruction) -> Vec<u32> {
    let mut reads = Vec::new();
    match instruction {
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

/// The exact root count a heap op at index `at` in `func` must declare as
/// `Live`: the number of contiguous `X` registers (`x0..`) holding a value that
/// is live ACROSS the heap op (defined before it, read at or after it before
/// being overwritten). GC clears every `X` at or above `Live` (§11.1 fact 4), so
/// this recomputed count is exactly what a correct `Live` operand equals (R8).
///
/// The scan models the register file directly: a write makes an `X` live; a call
/// leaves `x0` live and kills every higher `X`; a `Label` (a control-flow join)
/// and a heap op's own GC clear `X` at or above the declared `Live`.
pub(super) fn heap_live_root_count(func: &[Instruction], at: usize) -> u32 {
    let mut alive: BTreeSet<u32> = BTreeSet::new();
    for instruction in &func[..at] {
        apply_state(instruction, &mut alive);
    }
    let mut written_after: BTreeSet<u32> = BTreeSet::new();
    let mut roots: BTreeSet<u32> = BTreeSet::new();
    for (offset, instruction) in func[at..].iter().enumerate() {
        let (reads, writes) = reads_writes(instruction);
        for read in reads {
            if alive.contains(&read) && !written_after.contains(&read) {
                roots.insert(read);
            }
        }
        // The heap op's own destination write does not end liveness across it.
        if offset != 0 {
            for write in writes {
                written_after.insert(write);
            }
        }
        if offset != 0 && (is_call(instruction) || matches!(instruction, Instruction::Label { .. }))
        {
            break;
        }
    }
    roots.iter().next_back().map_or(0, |max| max + 1)
}

/// Advances the live-`X` set across one instruction (backward-pass state builder
/// for [`heap_live_root_count`]).
fn apply_state(instruction: &Instruction, alive: &mut BTreeSet<u32>) {
    if matches!(instruction, Instruction::Label { .. }) {
        alive.clear();
        return;
    }
    if let Some(live) = heap_live(instruction) {
        alive.retain(|&index| index < live);
    }
    if is_call(instruction) {
        alive.retain(|&index| index == 0);
        alive.insert(0);
        return;
    }
    for write in reads_writes(instruction).1 {
        alive.insert(write);
    }
}
