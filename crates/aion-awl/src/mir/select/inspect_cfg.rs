//! Control-flow-graph dataflow over one decoded function's instruction slice,
//! for the BC-5 codegen inspection. Everything here recomputes register-safety
//! facts from the REAL successors of the decoded stream — independently of the
//! emitter's own `Live` constants and register policy — so the §11/IR-14 oracles
//! catch a regression instead of restating it:
//!
//! * [`x_safety_violations`] — the cross-call / cross-join guarantee (BC-5
//!   review blocker 3): no live `X` value survives an `X`-clobbering call, and
//!   none survives a control-flow join in `X` without a redefinition on every
//!   path. A forward must-define fixed point (intersection at joins).
//! * [`heap_live_root_count`] — the `Live` a `TestHeap`/`GcBif` must declare
//!   (R8, BC-5 review blocker 4): the live-`X` root high-water at that op, from a
//!   real backward-liveness fixed point. A `Label` is a jump TARGET, never a
//!   register clobber — the old label-as-clobber model wrongly dropped a value
//!   that survives control flow in `X`, so the exact GC-safety regression under
//!   test could stay green.

use std::collections::{BTreeMap, BTreeSet};

use beamr::loader::decode::{Instruction, Operand};

use super::inspect_support::{is_call, reads_writes};

/// A set of `X` register indices.
type XSet = BTreeSet<u32>;

/// The `X` registers an instruction reads (BC-3's closed alphabet).
fn reads(instruction: &Instruction) -> XSet {
    reads_writes(instruction).0.into_iter().collect()
}

/// The `X` registers an instruction writes.
fn writes(instruction: &Instruction) -> XSet {
    reads_writes(instruction).1.into_iter().collect()
}

/// The `FuncInfo` arity of a decoded function slice (its incoming `X` argument
/// window `x0..x(arity-1)`), or `0` when none decodes.
fn entry_arity(func: &[Instruction]) -> u32 {
    func.iter()
        .find_map(|instruction| match instruction {
            Instruction::FuncInfo {
                arity: Operand::Unsigned(value),
                ..
            } => u32::try_from(*value).ok(),
            _ => None,
        })
        .unwrap_or(0)
}

/// The intra-function successor indices of every instruction in `func`.
///
/// A `Label` is a jump target (an edge INTO it), never a clobber. A non-tail
/// call returns to the following instruction (fall-through only — its callee
/// label is another function's entry). A `return`, an external tail call, or a
/// raise leaves the function (no in-function successor). A LOCAL tail call whose
/// label resolves inside this same slice is a loop back-edge (its resolved
/// target). A comparison/type-test/tagged-tuple test branches to its `fail`
/// label AND falls through; a `select_val` jumps to its `fail` label or a table
/// label (no fall-through); an unconditional `jump` transfers to its target.
fn successors(func: &[Instruction]) -> Vec<Vec<usize>> {
    let labels: BTreeMap<u32, usize> = func
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, index)),
            _ => None,
        })
        .collect();
    let resolve = |operand: &Operand| -> Option<usize> {
        match operand {
            Operand::Label(label) => labels.get(label).copied(),
            _ => None,
        }
    };
    let fallthrough = |index: usize| -> Vec<usize> {
        if index + 1 < func.len() {
            vec![index + 1]
        } else {
            Vec::new()
        }
    };
    let mut out = Vec::with_capacity(func.len());
    for (index, instruction) in func.iter().enumerate() {
        let succ = match instruction {
            Instruction::Return
            | Instruction::CallExtOnly { .. }
            | Instruction::CallExtLast { .. }
            | Instruction::Badmatch { .. }
            | Instruction::CaseEnd { .. } => Vec::new(),
            Instruction::Jump { target } => resolve(target).into_iter().collect(),
            Instruction::CallOnly { label, .. } | Instruction::CallLast { label, .. } => {
                resolve(label).into_iter().collect()
            }
            Instruction::Comparison { fail, .. }
            | Instruction::TypeTest { fail, .. }
            | Instruction::IsTaggedTuple { fail, .. } => {
                let mut succ = fallthrough(index);
                succ.extend(resolve(fail));
                succ
            }
            Instruction::SelectVal { fail, list, .. } => {
                let mut succ: Vec<usize> = resolve(fail).into_iter().collect();
                if let Operand::List(items) = list {
                    for item in items {
                        succ.extend(resolve(item));
                    }
                }
                succ
            }
            _ => fallthrough(index),
        };
        out.push(succ);
    }
    out
}

/// The predecessor indices of every instruction (the inverted successor graph).
fn predecessors(succ: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); succ.len()];
    for (index, targets) in succ.iter().enumerate() {
        for &target in targets {
            preds[target].push(index);
        }
    }
    preds
}

/// The register universe: every `X` index that appears, plus the argument
/// window — the `TOP` element the must-analysis narrows from.
fn register_universe(func: &[Instruction], arity: u32) -> XSet {
    let mut universe: XSet = (0..arity).collect();
    for instruction in func {
        let (read, written) = reads_writes(instruction);
        universe.extend(read);
        universe.extend(written);
    }
    universe
}

/// Whether an instruction is a tail call (a control transfer that re-enters a
/// function). Its only in-function successor is a loop back-edge — a re-entry of
/// THIS function (a different callee lands outside the slice), where the callee's
/// argument registers `x0..x(arity-1)` arrive freshly defined.
fn is_tail_call(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::CallOnly { .. }
            | Instruction::CallExtOnly { .. }
            | Instruction::CallLast { .. }
            | Instruction::CallExtLast { .. }
    )
}

/// Forward transfer for [`available_x`]: `avail_out = f(avail_in)`. A tail call's
/// in-function successor is a loop re-entry, so it presents the argument window
/// `x0..x(arity-1)` (the tail call marshaled them). A non-tail call clobbers
/// every `X` and leaves its result in `x0` for the fall-through. Any other op
/// adds its writes.
fn available_out(instruction: &Instruction, avail_in: &XSet, arity: u32) -> XSet {
    if is_tail_call(instruction) {
        return (0..arity).collect();
    }
    if is_call(instruction) {
        return std::iter::once(0).collect();
    }
    let mut out = avail_in.clone();
    out.extend(writes(instruction));
    out
}

/// The `X` registers guaranteed defined on EVERY path reaching each instruction,
/// given the arity-many argument registers arrive defined at entry (a forward
/// must-analysis; intersection at joins). Unreachable nodes stay `TOP`.
fn available_x(func: &[Instruction], arity: u32) -> Vec<XSet> {
    let count = func.len();
    let succ = successors(func);
    let preds = predecessors(&succ);
    let universe = register_universe(func, arity);
    let entry_defs: XSet = (0..arity).collect();
    let mut avail_in: Vec<XSet> = vec![universe.clone(); count];
    let mut changed = true;
    while changed {
        changed = false;
        for index in 0..count {
            let new_in = if index == 0 {
                entry_defs.clone()
            } else if preds[index].is_empty() {
                universe.clone()
            } else {
                let mut accumulated: Option<XSet> = None;
                for &pred in &preds[index] {
                    let out = available_out(&func[pred], &avail_in[pred], arity);
                    accumulated = Some(match accumulated {
                        None => out,
                        Some(acc) => acc.intersection(&out).copied().collect(),
                    });
                }
                accumulated.unwrap_or_else(|| entry_defs.clone())
            };
            if new_in != avail_in[index] {
                avail_in[index] = new_in;
                changed = true;
            }
        }
    }
    avail_in
}

/// Backward transfer for [`live_out_x`]: `live_in = f(live_out)`. A call is a
/// barrier — nothing but its argument registers is live just before it (it
/// clobbers every `X`, and its result in `x0` is freshly defined).
fn live_in(instruction: &Instruction, live_out: &XSet) -> XSet {
    if is_call(instruction) {
        return reads(instruction);
    }
    let mut in_set: XSet = live_out.difference(&writes(instruction)).copied().collect();
    in_set.extend(reads(instruction));
    in_set
}

/// The `X` registers live on exit from each instruction (a backward
/// may-analysis; union over successors), over the real control-flow successors.
fn live_out_x(func: &[Instruction]) -> Vec<XSet> {
    let count = func.len();
    let succ = successors(func);
    let mut live_out: Vec<XSet> = vec![XSet::new(); count];
    let mut changed = true;
    while changed {
        changed = false;
        for index in (0..count).rev() {
            let mut out = XSet::new();
            for &target in &succ[index] {
                out.extend(live_in(&func[target], &live_out[target]));
            }
            if out != live_out[index] {
                live_out[index] = out;
                changed = true;
            }
        }
    }
    live_out
}

/// The exact root count a heap op at index `at` in `func` must declare as `Live`
/// (R8): the contiguous-`X` high-water of the registers that are BOTH defined on
/// entry to the op AND read by the op or live on its exit — the values GC must
/// preserve across the potential collection (GC clears every `X` at or above
/// `Live`, §11.1 fact 4). Recomputed from a real backward-liveness fixed point
/// over the function's successors, so a value that survives a control-flow join
/// in `X` is counted (BC-5 review blocker 4).
pub(super) fn heap_live_root_count(func: &[Instruction], at: usize) -> u32 {
    let Some(instruction) = func.get(at) else {
        return 0;
    };
    let live_out = live_out_x(func);
    let avail = available_x(func, entry_arity(func));
    let mut live_here: XSet = reads(instruction);
    if let Some(out) = live_out.get(at) {
        live_here.extend(out.iter().copied());
    }
    let roots: XSet = match avail.get(at) {
        Some(defined) => live_here.intersection(defined).copied().collect(),
        None => XSet::new(),
    };
    roots.iter().next_back().map_or(0, |max| max + 1)
}

/// Every cross-call / cross-join `X`-safety violation in `func`: an `X` register
/// read where it is NOT defined on all paths since the function's arguments —
/// i.e. read after a call (or a control-flow join) without a redefinition on
/// every path. This is the real register-safety guarantee behind R8 (§11.1): no
/// live `X` value survives an `X`-clobbering call, and none survives a label in
/// `X` without a reload (BC-5 review blocker 3). Returns descriptions of each
/// violation; empty when safe. `make_fun` captures count as reads
/// (`inspect_support::reads_writes`), so a captured value surviving a call is
/// caught too.
pub(super) fn x_safety_violations(func: &[Instruction]) -> Vec<String> {
    let avail = available_x(func, entry_arity(func));
    let mut violations = Vec::new();
    for (index, instruction) in func.iter().enumerate() {
        let Some(defined) = avail.get(index) else {
            continue;
        };
        for register in reads(instruction) {
            if !defined.contains(&register) {
                violations.push(format!(
                    "x{register} is read by {instruction:?} but is not defined on all paths \
                     since the function's arguments (a value survived a call or a control-flow \
                     join in X without a redefinition)"
                ));
            }
        }
    }
    violations
}
