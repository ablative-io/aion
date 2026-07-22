//! Per-function structural oracles for the BC-5 codegen inspection, grounded on
//! facts carried INDEPENDENTLY into the decoded check — the input MIR's shape
//! and the module's own import/local target metadata — rather than restated from
//! the emitted instructions (BC-5 review blockers 5, 6, 7). The register-safety
//! facts (`X` never survives a call/join; `Live` = live-`X` high-water) come from
//! [`super::inspect_cfg`]; the framing/exit/marshaling facts here rest on them.

use std::collections::BTreeMap;

use beamr::atom::AtomTable;
use beamr::loader::decode::{Instruction, Operand};
use beamr::loader::load::ParsedModule;

use super::inspect_cfg::{heap_live_root_count, x_defined_at, x_safety_violations};
use super::inspect_support::{
    as_unsigned, heap_live, import_target, instruction_operands, is_call, operand_has_y,
    reads_writes, with_explicit_make_fun_reads,
};
use crate::mir::{Block, FlowFn, MirFn, Stmt, Tail};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The closed BC-3 instruction alphabet (`select/emit`). `Trim` is deliberately
/// absent (R6), so a membership check over the whole stream witnesses R6 and any
/// drift into an unexpected opcode fails loudly.
pub(super) fn is_known(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::Label { .. }
            | Instruction::FuncInfo { .. }
            | Instruction::Move { .. }
            | Instruction::Call { .. }
            | Instruction::CallOnly { .. }
            | Instruction::CallExt { .. }
            | Instruction::CallExtOnly { .. }
            | Instruction::CallLast { .. }
            | Instruction::CallExtLast { .. }
            | Instruction::CallFun { .. }
            | Instruction::Return
            | Instruction::Allocate { .. }
            | Instruction::Deallocate { .. }
            | Instruction::TestHeap { .. }
            | Instruction::PutList { .. }
            | Instruction::PutTuple2 { .. }
            | Instruction::GetTupleElement { .. }
            | Instruction::GetList { .. }
            | Instruction::TypeTest { .. }
            | Instruction::Comparison { .. }
            | Instruction::IsTaggedTuple { .. }
            | Instruction::SelectVal { .. }
            | Instruction::Jump { .. }
            | Instruction::Bif { .. }
            | Instruction::MakeFun { .. }
            | Instruction::Badmatch { .. }
            | Instruction::CaseEnd { .. }
    )
}

/// Whether an instruction is a frame teardown-carrying tail call.
pub(super) fn is_framed_tail(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::CallLast { .. } | Instruction::CallExtLast { .. }
    )
}

/// Whether an instruction is a frameless tail call.
pub(super) fn is_frameless_tail(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::CallOnly { .. } | Instruction::CallExtOnly { .. }
    )
}

/// The `deallocate` word count a framed tail call declares.
fn framed_tail_words(instruction: &Instruction) -> Option<u64> {
    match instruction {
        Instruction::CallLast { deallocate, .. } | Instruction::CallExtLast { deallocate, .. } => {
            as_unsigned(deallocate)
        }
        _ => None,
    }
}

/// The call arity an instruction self-declares, if it is a call.
fn call_arity(instruction: &Instruction) -> Option<u32> {
    let (Instruction::Call { arity, .. }
    | Instruction::CallOnly { arity, .. }
    | Instruction::CallExt { arity, .. }
    | Instruction::CallExtOnly { arity, .. }
    | Instruction::CallLast { arity, .. }
    | Instruction::CallExtLast { arity, .. }
    | Instruction::CallFun { arity }) = instruction
    else {
        return None;
    };
    as_unsigned(arity).and_then(|value| u32::try_from(value).ok())
}

/// The `arity` operand of a function's `FuncInfo`.
pub(super) fn function_arity(code: &[Instruction]) -> Option<u32> {
    code.iter().find_map(|instruction| match instruction {
        Instruction::FuncInfo { arity, .. } => {
            as_unsigned(arity).and_then(|value| u32::try_from(value).ok())
        }
        _ => None,
    })
}

/// A map from a decoded function's BODY label (the `Label` after its `FuncInfo`,
/// the target a local call transfers to) to its declared arity — the independent
/// metadata a local call's arity is checked against.
pub(super) fn local_target_arities(parsed: &ParsedModule) -> BTreeMap<u32, u32> {
    let stream = &parsed.instructions;
    let mut map = BTreeMap::new();
    for index in 0..stream.len() {
        let Instruction::FuncInfo { arity, .. } = &stream[index] else {
            continue;
        };
        let Some(arity) = as_unsigned(arity).and_then(|value| u32::try_from(value).ok()) else {
            continue;
        };
        if let Some(Instruction::Label { label }) = stream.get(index + 1) {
            map.insert(*label, arity);
        }
    }
    map
}

/// Every per-function IR-14/§11 invariant for one decoded function, with the
/// module's target metadata available for arity resolution.
pub(super) fn check_function(
    label: &str,
    name: &str,
    code: &[Instruction],
    parsed: &ParsedModule,
    table: &AtomTable,
    local_arities: &BTreeMap<u32, u32>,
) -> TestResult {
    let where_ = format!("{label}::{name}");
    // Make each `make_fun2`'s implicit capture reads `x0..x(num_free-1)` explicit
    // (resolved through the module's `FunT`) before any register analysis, so a
    // deleted or stale capture reload is visible to the CFG (BC-5 review
    // blocker 3). All other instructions are untouched, so the framing/exit/
    // marshaling checks below see the real decoded shape.
    let analysis = with_explicit_make_fun_reads(code, &parsed.lambdas)?;
    let code = analysis.as_slice();
    let arity = function_arity(code)
        .ok_or_else(|| format!("{where_}: function has no decodable FuncInfo arity"))?;
    let framed = code
        .iter()
        .any(|instruction| matches!(instruction, Instruction::Allocate { .. }));

    // R8 register safety (BC-5 review blocker 3): no live X value survives an
    // X-clobbering call, and none survives a control-flow join in X without a
    // reload — recomputed from the real successors, not assumed.
    let violations = x_safety_violations(code);
    if let Some(first) = violations.first() {
        return Err(format!("{where_}: R8 X-safety — {first}").into());
    }

    if framed {
        check_framed(code, arity, &where_)?;
    } else {
        check_frameless(code, arity, &where_)?;
    }

    // R8 `Live` accuracy: every heap op's `Live` equals the recomputed live-`X`
    // root high-water at that op (BC-5 review blocker 4).
    assert_live_accuracy(&where_, code)?;

    check_call_convention(code, parsed, table, local_arities, &where_)?;
    Ok(())
}

/// R8 `Live` accuracy over one function slice: every `TestHeap`/`GcBif` heap op's
/// declared `Live` equals the recomputed live-`X` root high-water at that op (GC
/// clears `X` at/above `Live`, §11.1 fact 4). Exposed so the oracle-mutation test
/// can prove a deliberately wrong `Live` goes red.
///
/// # Errors
///
/// Returns an error naming the first heap op whose declared `Live` differs from
/// the recomputed high-water.
pub(super) fn assert_live_accuracy(where_: &str, code: &[Instruction]) -> TestResult {
    for (index, instruction) in code.iter().enumerate() {
        if let Some(declared) = heap_live(instruction) {
            let computed = heap_live_root_count(code, index);
            if declared != computed {
                return Err(format!(
                    "{where_}: R8 `Live` drift at {instruction:?}: declared {declared}, \
                     live-X high-water {computed}"
                )
                .into());
            }
        }
    }
    Ok(())
}

/// Framed-function discipline (R5 tier-2, R7 single exit, R8 predicate) with the
/// prologue pinned EXACTLY: a single leading `Allocate F` (F ≥ 1) whose `live`
/// operand equals the arity, then the arity-many spills `move x_i → y_i` with
/// BOTH the source AND destination register index pinned to `i` (BC-5 review
/// blocker 5 — the destination is no longer wildcarded), no frameless tail, every
/// framed tail deallocating exactly `F`, and one linearly-last `Deallocate F;
/// Return` when any `Return` exists.
pub(super) fn check_framed(code: &[Instruction], arity: u32, where_: &str) -> TestResult {
    if !(matches!(code.first(), Some(Instruction::Label { .. }))
        && matches!(code.get(1), Some(Instruction::FuncInfo { .. }))
        && matches!(code.get(2), Some(Instruction::Label { .. })))
    {
        return Err(format!("{where_}: framed header is not Label/FuncInfo/Label").into());
    }
    let (frame_size, live) = match code.get(3) {
        Some(Instruction::Allocate { stack_need, live }) => (
            as_unsigned(stack_need)
                .ok_or_else(|| format!("{where_}: Allocate stack_need is not a count"))?,
            as_unsigned(live).ok_or_else(|| format!("{where_}: Allocate live is not a count"))?,
        ),
        other => {
            return Err(
                format!("{where_}: framed body does not open with Allocate: {other:?}").into(),
            );
        }
    };
    if frame_size < 1 {
        return Err(format!("{where_}: R8 predicate — framed frame_size must be > 0").into());
    }
    if live != u64::from(arity) {
        return Err(format!(
            "{where_}: Allocate live {live} does not equal the arity {arity} (the args being spilled)"
        )
        .into());
    }
    if code
        .iter()
        .filter(|instruction| matches!(instruction, Instruction::Allocate { .. }))
        .count()
        != 1
    {
        return Err(format!("{where_}: a framed function has exactly one Allocate").into());
    }

    // Prologue: the arity-many params spill x_i -> y_i, index pinned on BOTH ends.
    for index in 0..arity {
        let slot = 4 + index as usize;
        let ok = matches!(
            code.get(slot),
            Some(Instruction::Move {
                source: Operand::X(source),
                destination: Operand::Y(destination),
            }) if *source == index && *destination == index
        );
        if !ok {
            return Err(format!(
                "{where_}: prologue spill {index} is not `move x{index} -> y{index}` (got {:?})",
                code.get(slot)
            )
            .into());
        }
    }

    for instruction in code {
        if is_frameless_tail(instruction) {
            return Err(format!(
                "{where_}: framed function used a frameless tail: {instruction:?}"
            )
            .into());
        }
        if let Some(words) = framed_tail_words(instruction)
            && words != frame_size
        {
            return Err(format!(
                "{where_}: framed tail deallocates {words}, not the frame size {frame_size}"
            )
            .into());
        }
    }

    check_framed_exit(code, frame_size, where_)
}

/// R7 single shared exit for a framed function: at most one standalone
/// `Deallocate`; when present it releases the frame size, is immediately followed
/// by `Return`, is linearly last, and no `Y` operand survives it. EVERY `Return`
/// is a deallocated return, and the value it returns is defined in `x0` at that
/// point (BC-5 review blocker 5 — result placement).
fn check_framed_exit(code: &[Instruction], frame_size: u64, where_: &str) -> TestResult {
    let deallocs: Vec<usize> = code
        .iter()
        .enumerate()
        .filter(|(_, instruction)| matches!(instruction, Instruction::Deallocate { .. }))
        .map(|(index, _)| index)
        .collect();
    if deallocs.len() > 1 {
        return Err(format!("{where_}: R7 — more than one Deallocate").into());
    }
    if let Some(&at) = deallocs.first() {
        let releases_frame = matches!(code.get(at), Some(Instruction::Deallocate { words })
            if as_unsigned(words) == Some(frame_size));
        if !releases_frame {
            return Err(format!("{where_}: the Deallocate does not release the frame size").into());
        }
        if !matches!(code.get(at + 1), Some(Instruction::Return)) {
            return Err(
                format!("{where_}: R7 — Deallocate is not immediately followed by Return").into(),
            );
        }
        if at + 2 != code.len() {
            return Err(format!("{where_}: R7 — Deallocate/Return is not linearly last").into());
        }
        for instruction in &code[at + 1..] {
            if instruction_operands(instruction)
                .iter()
                .any(|operand| operand_has_y(operand))
            {
                return Err(format!("{where_}: a Y operand survives the Deallocate").into());
            }
        }
    }
    for (index, instruction) in code.iter().enumerate() {
        if matches!(instruction, Instruction::Return) {
            if index == 0 || !matches!(code.get(index - 1), Some(Instruction::Deallocate { .. })) {
                return Err(format!(
                    "{where_}: R7 — a Return in a framed function is not preceded by Deallocate"
                )
                .into());
            }
            // The returned value is in `x0` by ABI: `x0` must be defined on EVERY
            // path reaching this exit — the forward must-define fixed point over
            // the whole function (intersection at joins), not the prefix-wide
            // no-violations proxy that a stale unrelated `x0` could satisfy (BC-5
            // review blocker 5). The exact reload identity is pinned on the
            // targeted fixtures (`framed_return_reloads_the_return_value_into_x0`).
            if !x_defined_at(code, index, 0) {
                return Err(format!(
                    "{where_}: the return value is not defined in x0 on all paths to the exit"
                )
                .into());
            }
        }
    }
    Ok(())
}

/// Frameless-body discipline (R5 tier-1, R8 predicate): no `Allocate`, no
/// `Deallocate`, no `Y` operand, no framed tail, and — since any parameter makes
/// `frame_size > 0` — arity zero. A should-be-framed body that regressed to
/// keeping values in `X` across a call is caught independently by the CFG
/// X-safety check in [`check_function`] (BC-5 review blocker 5).
pub(super) fn check_frameless(code: &[Instruction], arity: u32, where_: &str) -> TestResult {
    if arity != 0 {
        return Err(format!(
            "{where_}: R8 predicate — a frameless function must take no parameters"
        )
        .into());
    }
    for instruction in code {
        if matches!(
            instruction,
            Instruction::Allocate { .. } | Instruction::Deallocate { .. }
        ) {
            return Err(format!(
                "{where_}: frameless function carries a frame instruction: {instruction:?}"
            )
            .into());
        }
        if is_framed_tail(instruction) {
            return Err(format!(
                "{where_}: frameless function used a framed tail: {instruction:?}"
            )
            .into());
        }
        if instruction_operands(instruction)
            .iter()
            .any(|operand| operand_has_y(operand))
        {
            return Err(format!(
                "{where_}: R5 tier-1 — a frameless body names a Y register: {instruction:?}"
            )
            .into());
        }
    }
    Ok(())
}

/// IR-14 marshaling and result placement (BC-5 review blocker 6): each call's
/// declared arity equals its TARGET's metadata arity (an external import's `ImpT`
/// arity, or a local callee's own `FuncInfo` arity — never the call's own
/// self-declared operand); its argument registers `x0..x(k-1)` (a `CallFun` also
/// `x(k)` for the fun) are each defined at the call site (a reaching definition,
/// via the CFG availability the X-safety check computes); and a value-producing
/// call whose result is immediately stored reads that result from `x0`.
pub(super) fn check_call_convention(
    code: &[Instruction],
    parsed: &ParsedModule,
    table: &AtomTable,
    local_arities: &BTreeMap<u32, u32>,
    where_: &str,
) -> TestResult {
    for (index, instruction) in code.iter().enumerate() {
        let Some(declared_arity) = call_arity(instruction) else {
            continue;
        };
        if let Some(target_arity) = target_arity(instruction, parsed, table, local_arities)
            && target_arity != declared_arity
        {
            return Err(format!(
                "{where_}: IR-14 — call declares arity {declared_arity} but its target takes \
                 {target_arity}: {instruction:?}"
            )
            .into());
        }
        let required: Vec<u32> = if matches!(instruction, Instruction::CallFun { .. }) {
            (0..=declared_arity).collect()
        } else {
            (0..declared_arity).collect()
        };
        // Reaching definition: each argument register is written in the marshal
        // segment since the previous call/label, so no stale write is accepted.
        let mut marshaled: Vec<u32> = Vec::new();
        for prior in code[..index].iter().rev() {
            if is_call(prior) || matches!(prior, Instruction::Label { .. }) {
                break;
            }
            marshaled.extend(reads_writes(prior).1);
        }
        for register in &required {
            if !marshaled.contains(register) {
                return Err(format!(
                    "{where_}: IR-14 — call arg x{register} (arity {declared_arity}) is not \
                     marshaled in the block before the call: {instruction:?}"
                )
                .into());
            }
        }
        // Reaching-def strengthening: the args must also be defined on every path
        // into the call (they are, by construction of the marshal block above and
        // the CFG availability the X-safety check already proved for `code`).
        if let Some(Instruction::Move { source, .. }) = code.get(index + 1)
            && let Operand::X(source_index) = source
            && *source_index != 0
        {
            return Err(format!(
                "{where_}: IR-14 — a result store right after a call reads x{source_index}, not x0"
            )
            .into());
        }
    }
    Ok(())
}

/// The arity the decoded call's TARGET declares, resolved independently of the
/// call's own operand: an external import's `ImpT` arity, or a local callee's
/// `FuncInfo` arity via its body label.
fn target_arity(
    instruction: &Instruction,
    parsed: &ParsedModule,
    table: &AtomTable,
    local_arities: &BTreeMap<u32, u32>,
) -> Option<u32> {
    match instruction {
        Instruction::CallExt { import, .. }
        | Instruction::CallExtOnly { import, .. }
        | Instruction::CallExtLast { import, .. } => {
            import_target(parsed, table, import).map(|(_, _, arity)| u32::from(arity))
        }
        Instruction::Call { label, .. }
        | Instruction::CallOnly { label, .. }
        | Instruction::CallLast { label, .. } => match label {
            Operand::Label(target) => local_arities.get(target).copied(),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a MIR flow function is EXPECTED to be framed (R8's `frame_size > 0`
/// predicate, derived from the input): it has ≥1 param, or its body defines ≥1
/// var (params spill to `Y`; every def takes a `Y` home). A function with neither
/// is frameless (BC-5 review blocker 5).
pub(super) fn expected_framed(flow: &FlowFn) -> bool {
    !flow.params.is_empty() || block_defines_var(&flow.body)
}

/// Whether a MIR flow function's body ends any path in a plain `Return` (a
/// non-tail return) — the independent fact that a framed function MUST close on a
/// `Deallocate; Return` exit (BC-5 review blocker 5).
pub(super) fn flow_returns(flow: &FlowFn) -> bool {
    block_returns(&flow.body)
}

/// Whether a block (recursively through control tails) defines any var.
fn block_defines_var(block: &Block) -> bool {
    block.stmts.iter().any(stmt_defines_var) || tail_defines_var(&block.tail)
}

/// Whether a statement defines a var (or, for `AssertList`/`Attempt`, binds one).
fn stmt_defines_var(stmt: &Stmt) -> bool {
    if stmt.defined().is_some() {
        return true;
    }
    match stmt {
        Stmt::AssertList { binds, .. } => binds.iter().any(Option::is_some),
        Stmt::Attempt {
            defs,
            on_ok,
            on_err,
            ..
        } => !defs.is_empty() || block_defines_var(on_ok) || block_defines_var(on_err),
        _ => false,
    }
}

/// Whether a tail's nested blocks define any var.
fn tail_defines_var(tail: &Tail) -> bool {
    match tail {
        Tail::If {
            then_block,
            else_block,
            ..
        } => block_defines_var(then_block) || block_defines_var(else_block),
        Tail::SelectEnum { arms, .. } => arms.iter().any(|(_, block)| block_defines_var(block)),
        _ => false,
    }
}

/// Whether a block ends any path in a plain `Return`.
fn block_returns(block: &Block) -> bool {
    match &block.tail {
        Tail::Return(_) => true,
        Tail::If {
            then_block,
            else_block,
            ..
        } => block_returns(then_block) || block_returns(else_block),
        Tail::SelectEnum { arms, .. } => arms.iter().any(|(_, block)| block_returns(block)),
        Tail::TailLocal { .. } | Tail::TailRt { .. } => false,
    }
}

/// The `MirFn` in `module` with a given decoded name, when it is a flow
/// function (the shapes carrying independent framing expectations).
pub(super) fn flow_named<'m>(functions: &'m [MirFn], name: &str) -> Option<&'m FlowFn> {
    functions.iter().find_map(|function| match function {
        MirFn::Flow(flow) if flow.name == name => Some(flow),
        _ => None,
    })
}
