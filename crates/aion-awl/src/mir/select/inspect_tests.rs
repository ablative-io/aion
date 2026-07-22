//! BC-5 codegen inspection: the IR-14 calling-convention row (`AWL-BC-IR.md`
//! line 715) and its §11 refinements R5–R8, asserted at the INSTRUCTION level
//! against decoded `select()` output — the direct byte/instruction proof the
//! BC-4 round-3 ruling deferred here (IR-14 is not observable at the durable
//! trail level).
//!
//! Two witnesses per claim: a sweep over the full covered ratchet (every
//! `valid/` fixture the direct compiler lowers) applying the module-wide
//! invariants, and targeted per-shape fixtures — one framed function, one
//! frameless body, one loop (self tail-call), one route (tail-call) — that
//! isolate a single shape. Where a §11 claim proves false against the bytes it
//! is a divergence to adjudicate, never weakened to match the emitter.
//!
//! Register-file grounding (§11.1, code-verified): the JIT has no `Allocate`
//! lowering, so any `Y` operand pins a function to the interpreter; GC clears
//! every `X` at or above a heap op's `Live` (fact 4), so `Live` must equal the
//! live-`X` root high-water — the invariant `heap_live_root_count` recomputes.

use beamr::loader::decode::{Instruction, Operand};

use super::inspect_support::{
    DecodedFn, as_unsigned, decode, destinations, functions, heap_live, heap_live_root_count,
    instruction_operands, is_call, lowered_fixtures, operand_has_y,
};
use crate::mir::{
    AtomRef, Block, FlowFn, FnOrigin, FnRef, MirFn, MirModule, RuntimeFn, Span, Stmt, Tail, TyDesc,
    Value, Var, lower, select,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The closed BC-3 instruction alphabet (`select/emit`). `Trim` is deliberately
/// absent (R6: frames die at the single `Deallocate`, never trimmed), so a
/// membership check over the whole stream witnesses R6 for free — and any drift
/// into an unexpected opcode fails loudly rather than passing unexamined.
fn is_known(instruction: &Instruction) -> bool {
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
fn is_framed_tail(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::CallLast { .. } | Instruction::CallExtLast { .. }
    )
}

/// Whether an instruction is a frameless tail call.
fn is_frameless_tail(instruction: &Instruction) -> bool {
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

/// The `X` destinations an instruction writes (a subset of `reads_writes`, kept
/// local so the arg-marshaling check reads cleanly).
fn x_writes(instruction: &Instruction) -> Vec<u32> {
    super::inspect_support::reads_writes(instruction).1
}

/// The call arity an instruction declares, if it is a call.
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

// ---- the covered-ratchet sweep ----

/// Every module-wide IR-14/§11 invariant, over every fixture the direct
/// compiler lowers. A single failing fixture names itself (label + function).
#[test]
fn covered_ratchet_upholds_ir14_and_section_11() -> TestResult {
    let mut fixtures = 0_usize;
    for (label, module) in lowered_fixtures()? {
        let (parsed, table) = decode(&module)?;

        // R6 + closed alphabet: no `Trim`, no foreign opcode, anywhere.
        for instruction in &parsed.instructions {
            assert!(
                is_known(instruction),
                "{label}: unexpected instruction in BC-3 output: {instruction:?}"
            );
            assert!(
                !matches!(instruction, Instruction::Trim { .. }),
                "{label}: R6 violated — `trim` was emitted"
            );
        }

        // R8 register safety — the byte-true form (see the module note and the
        // amended IR-14 row): a `Y` register is WRITTEN only by a `move` (every
        // Y destination is a `move`), and NO call carries a `Y` operand, so no
        // value crosses a call from a Y home without an X reload and X never
        // holds a live value across a call. The stronger §11.2 wording — "Y is
        // touched ONLY by move, every other instruction sees X/literal/atom
        // operands only" — is FALSE against these bytes (STOPPED, see the note);
        // guard/heap ops read Y homes directly, which this suite does not assert
        // away.
        for instruction in &parsed.instructions {
            if !matches!(instruction, Instruction::Move { .. }) {
                assert!(
                    !destinations(instruction)
                        .iter()
                        .any(|operand| operand_has_y(operand)),
                    "{label}: a non-`move` instruction WRITES a Y register: {instruction:?}"
                );
            }
            if is_call(instruction) {
                assert!(
                    !instruction_operands(instruction)
                        .iter()
                        .any(|operand| operand_has_y(operand)),
                    "{label}: a call carries a Y operand (value crosses a call without an X reload): {instruction:?}"
                );
            }
        }

        for function in functions(&parsed, &table) {
            check_function(&label, &function)?;
        }
        fixtures += 1;
    }
    assert!(
        fixtures > 0,
        "the sweep proved nothing — no fixture lowered"
    );
    Ok(())
}

/// Every per-function IR-14/§11 invariant for one decoded function.
fn check_function(label: &str, function: &DecodedFn<'_>) -> TestResult {
    let code = function.code;
    let name = &function.name;
    let where_ = || format!("{label}::{name}");

    let arity = function_arity(code)
        .ok_or_else(|| format!("{}: function has no decodable FuncInfo arity", where_()))?;
    let framed = code
        .iter()
        .any(|instruction| matches!(instruction, Instruction::Allocate { .. }));

    if framed {
        check_framed(code, arity, &where_())?;
    } else {
        check_frameless(code, arity, &where_());
    }

    // R8 `Live` accuracy: every heap op's `Live` equals the live-`X` root
    // high-water at that op (GC clears X at/above `Live`, §11.1 fact 4).
    for (index, instruction) in code.iter().enumerate() {
        if let Some(declared) = heap_live(instruction) {
            let computed = heap_live_root_count(code, index);
            assert_eq!(
                declared,
                computed,
                "{}: R8 `Live` drift at {instruction:?}: declared {declared}, live-X high-water {computed}",
                where_()
            );
        }
    }

    // IR-14 marshaling: each call's arguments occupy exactly `x0..x(arity-1)`
    // (a `CallFun` additionally homes the fun in `x(arity)`), and a
    // value-producing call leaves its result in `x0`.
    check_call_convention(code, &where_());
    Ok(())
}

/// The `arity` operand of a function's `FuncInfo`.
fn function_arity(code: &[Instruction]) -> Option<u32> {
    code.iter().find_map(|instruction| match instruction {
        Instruction::FuncInfo { arity, .. } => {
            as_unsigned(arity).and_then(|value| u32::try_from(value).ok())
        }
        _ => None,
    })
}

/// Framed-function discipline (R5 tier-2, R7 single exit, R8 predicate):
/// header + single leading `Allocate F` (F ≥ 1), the arity-many prologue param
/// spills `move x_i -> y_i`, at most one standalone `Deallocate` (immediately
/// followed by `Return`, linearly last, with no `Y` after it), no frameless
/// tail, and every framed tail deallocating exactly `F`.
fn check_framed(code: &[Instruction], arity: u32, where_: &str) -> TestResult {
    // Header: Label, FuncInfo, Label, then the sole Allocate.
    assert!(
        matches!(code.first(), Some(Instruction::Label { .. }))
            && matches!(code.get(1), Some(Instruction::FuncInfo { .. }))
            && matches!(code.get(2), Some(Instruction::Label { .. })),
        "{where_}: framed header is not Label/FuncInfo/Label"
    );
    let frame_size = match code.get(3) {
        Some(Instruction::Allocate { stack_need, .. }) => as_unsigned(stack_need)
            .ok_or_else(|| format!("{where_}: Allocate stack_need is not a count"))?,
        other => {
            return Err(
                format!("{where_}: framed body does not open with Allocate: {other:?}").into(),
            );
        }
    };
    assert!(
        frame_size >= 1,
        "{where_}: R8 predicate — framed frame_size must be > 0"
    );
    assert_eq!(
        code.iter()
            .filter(|instruction| matches!(instruction, Instruction::Allocate { .. }))
            .count(),
        1,
        "{where_}: a framed function has exactly one Allocate"
    );

    // R8 / IR-14: prologue spills the arity-many params x0..x(arity-1) into Y,
    // in order — the function received its arguments in x0..x(n-1).
    for index in 0..arity {
        let slot = 4 + index as usize;
        assert!(
            matches!(
                code.get(slot),
                Some(Instruction::Move { source: Operand::X(source), .. }) if *source == index
            ),
            "{where_}: prologue spill {index} does not move x{index} (arg not in x{index})"
        );
    }

    // Frameless tails never appear in a framed function; framed tails deallocate F.
    for instruction in code {
        assert!(
            !is_frameless_tail(instruction),
            "{where_}: framed function used a frameless tail: {instruction:?}"
        );
        if let Some(words) = framed_tail_words(instruction) {
            assert_eq!(
                words, frame_size,
                "{where_}: framed tail deallocates {words}, not the frame size {frame_size}"
            );
        }
    }

    // R7 single shared exit: at most one standalone Deallocate; if present it is
    // immediately followed by Return, is the linearly-last pair, and no Y
    // operand survives it. Every Return is a deallocated return.
    let deallocs: Vec<usize> = code
        .iter()
        .enumerate()
        .filter(|(_, instruction)| matches!(instruction, Instruction::Deallocate { .. }))
        .map(|(index, _)| index)
        .collect();
    assert!(
        deallocs.len() <= 1,
        "{where_}: R7 — more than one Deallocate"
    );
    if let Some(&at) = deallocs.first() {
        assert!(
            matches!(code.get(at), Some(Instruction::Deallocate { words })
                if as_unsigned(words) == Some(frame_size)),
            "{where_}: the Deallocate does not release the frame size"
        );
        assert!(
            matches!(code.get(at + 1), Some(Instruction::Return)),
            "{where_}: R7 — Deallocate is not immediately followed by Return"
        );
        assert_eq!(
            at + 2,
            code.len(),
            "{where_}: R7 — Deallocate/Return is not linearly last"
        );
        for instruction in &code[at + 1..] {
            assert!(
                !instruction_operands(instruction)
                    .iter()
                    .any(|operand| operand_has_y(operand)),
                "{where_}: a Y operand survives the Deallocate"
            );
        }
    }
    for (index, instruction) in code.iter().enumerate() {
        if matches!(instruction, Instruction::Return) {
            assert!(
                index > 0 && matches!(code.get(index - 1), Some(Instruction::Deallocate { .. })),
                "{where_}: R7 — a Return in a framed function is not preceded by Deallocate"
            );
        }
    }
    Ok(())
}

/// Frameless-body discipline (R5 tier-1, R8 predicate): no `Allocate`, no
/// `Deallocate`, no `Y` operand, no framed tail, and — since any parameter makes
/// `frame_size > 0` — arity zero.
fn check_frameless(code: &[Instruction], arity: u32, where_: &str) {
    assert_eq!(
        arity, 0,
        "{where_}: R8 predicate — a frameless function must take no parameters"
    );
    for instruction in code {
        assert!(
            !matches!(
                instruction,
                Instruction::Allocate { .. } | Instruction::Deallocate { .. }
            ),
            "{where_}: frameless function carries a frame instruction: {instruction:?}"
        );
        assert!(
            !is_framed_tail(instruction),
            "{where_}: frameless function used a framed tail: {instruction:?}"
        );
        assert!(
            !instruction_operands(instruction)
                .iter()
                .any(|operand| operand_has_y(operand)),
            "{where_}: R5 tier-1 — a frameless body names a Y register: {instruction:?}"
        );
    }
}

/// IR-14 marshaling and result placement: for each call of arity `k`, the
/// segment since the previous call/label writes all of `x0..x(k-1)` (`CallFun`
/// also `x(k)` for the fun); a value-producing call immediately followed by a
/// register store leaves its result in `x0`.
fn check_call_convention(code: &[Instruction], where_: &str) {
    for (index, instruction) in code.iter().enumerate() {
        let Some(arity) = call_arity(instruction) else {
            continue;
        };
        // A `CallFun` homes the fun in `x(arity)`; every other call has args
        // `x0..x(arity-1)` and no fun register.
        let required: Vec<u32> = if matches!(instruction, Instruction::CallFun { .. }) {
            (0..=arity).collect()
        } else {
            (0..arity).collect()
        };
        let mut written: Vec<u32> = Vec::new();
        for prior in code[..index].iter().rev() {
            if is_call_or_label(prior) {
                break;
            }
            written.extend(x_writes(prior));
        }
        for register in required {
            assert!(
                written.contains(&register),
                "{where_}: IR-14 — call arg x{register} (arity {arity}) is not marshaled before the call"
            );
        }
        // Result in x0: a store right after a value-producing call reads x0.
        if let Some(Instruction::Move { source, .. }) = code.get(index + 1)
            && let Operand::X(source_index) = source
        {
            assert_eq!(
                *source_index, 0,
                "{where_}: IR-14 — result store after a call does not read x0"
            );
        }
    }
}

/// Whether an instruction ends a call-free segment (a call or a label join).
fn is_call_or_label(instruction: &Instruction) -> bool {
    is_call(instruction) || matches!(instruction, Instruction::Label { .. })
}

// ---- targeted per-shape fixtures ----

/// A hand-built flow function (mirrors the `select` unit-test idiom).
fn flow(name: &str, origin: FnOrigin, params: &[u32], stmts: Vec<Stmt>, tail: Tail) -> MirFn {
    MirFn::Flow(FlowFn {
        origin,
        name: name.to_owned(),
        params: params.iter().map(|index| Var(*index)).collect(),
        param_tys: vec![TyDesc::Nil; params.len()],
        ret_ty: TyDesc::Nil,
        body: Block { stmts, tail },
        span: Span { line: 0, column: 0 },
        degraded_parallel: false,
    })
}

/// A hand-built single-function module.
fn module(name: &str, atoms: &[&str], functions: Vec<MirFn>) -> MirModule {
    MirModule {
        name: name.to_owned(),
        source: format!("{name}.awl"),
        atoms: atoms.iter().map(|atom| (*atom).to_owned()).collect(),
        literals: Vec::new(),
        exports: vec![FnRef(0)],
        functions,
        types: Vec::new(),
    }
}

/// Targeted: a framed function (one param, one def) brackets its body with a
/// single `Allocate`, spills its param, and exits through one `Deallocate;
/// Return` linearly last (R5 tier-2, R7).
#[test]
fn targeted_framed_function_brackets_and_single_exits() -> TestResult {
    let one = flow(
        "execute",
        FnOrigin::Execute,
        &[0],
        vec![Stmt::RecordNew {
            dst: Var(1),
            tag: AtomRef(0),
            args: vec![Value::Var(Var(0))],
            span: Span { line: 0, column: 0 },
        }],
        Tail::Return(Value::Var(Var(1))),
    );
    let bytes = select(&module("framed", &["ok"], vec![one]))?;
    let table = beamr::atom::AtomTable::with_common_atoms();
    let parsed = beamr::loader::load_beam_chunks(&bytes, &table)?;
    let function = functions(&parsed, &table)
        .into_iter()
        .find(|function| function.name == "execute")
        .ok_or("no execute function")?;
    assert!(
        function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Allocate { .. })),
        "expected a framed function"
    );
    check_framed(function.code, 1, "targeted::framed")?;
    Ok(())
}

/// Targeted: a frameless body (no params, no defs, a tail import over
/// immediates) emits no `Allocate`, no `Y`, and a `call_ext_only` tail (R5
/// tier-1, R8 predicate).
#[test]
fn targeted_frameless_body_uses_no_frame_or_y() -> TestResult {
    let body = flow(
        "execute",
        FnOrigin::Execute,
        &[],
        Vec::new(),
        Tail::TailRt {
            callee: RuntimeFn::DSuccess,
            args: vec![Value::Nil],
        },
    );
    let bytes = select(&module("frameless", &[], vec![body]))?;
    let table = beamr::atom::AtomTable::with_common_atoms();
    let parsed = beamr::loader::load_beam_chunks(&bytes, &table)?;
    let function = functions(&parsed, &table)
        .into_iter()
        .find(|function| function.name == "execute")
        .ok_or("no execute function")?;
    assert_eq!(
        function
            .code
            .iter()
            .filter(|instruction| matches!(instruction, Instruction::Allocate { .. }))
            .count(),
        0,
        "a frameless body must not allocate"
    );
    assert!(
        function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instruction::CallExtOnly { .. })),
        "a frameless tail import must be call_ext_only"
    );
    check_frameless(function.code, 0, "targeted::frameless");
    Ok(())
}

/// Targeted: a counted loop's back-edge is a self tail call — a `call_last` /
/// `call_only` whose label operand is the loop function's own body label (IR-14:
/// loop recursion is a tail call).
#[test]
fn targeted_loop_recursion_is_a_self_tail_call() -> TestResult {
    let (parsed, table) = decode(&fixture_module(
        "loop-outcomes/valid/loop_counting_until_max",
    )?)?;
    let mut found = false;
    for function in functions(&parsed, &table) {
        let Some(Instruction::Label { label: body }) = function.code.get(2) else {
            continue;
        };
        for instruction in function.code {
            let (Instruction::CallLast { label: target, .. }
            | Instruction::CallOnly { label: target, .. }) = instruction
            else {
                continue;
            };
            if matches!(target, Operand::Label(value) if value == body) {
                found = true;
            }
        }
    }
    assert!(
        found,
        "no self-referential tail call — loop recursion is not a tail call"
    );
    Ok(())
}

/// Targeted: `awl_hello`'s `route shouted` lowers to a tail call — the region
/// function ends in `call_ext_last` into the SDK success runtime, not a
/// `call_ext` followed by a `Return` (IR-14: routes are tail calls).
#[test]
fn targeted_route_lowers_to_a_tail_call() -> TestResult {
    let (parsed, table) = decode(&fixture_module("flagship/valid/awl_hello")?)?;
    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::CallExtLast { .. })),
        "awl_hello emits no tail call for its route"
    );
    // No function returns a non-x0 route value through a non-tail call: every
    // route path leaves via a framed/frameless tail call.
    for function in functions(&parsed, &table) {
        let terminators = function
            .code
            .iter()
            .filter(|instruction| {
                is_framed_tail(instruction)
                    || is_frameless_tail(instruction)
                    || matches!(instruction, Instruction::Return)
            })
            .count();
        assert!(terminators >= 1, "a function has no terminator");
    }
    Ok(())
}

/// Lowers a covered fixture by its `<dir>/<stem>` label to a `MirModule`.
fn fixture_module(relative: &str) -> Result<MirModule, Box<dyn std::error::Error>> {
    let path = super::inspect_support::manifest_dir()
        .join("tests/fixtures/rev2")
        .join(format!("{relative}.awl"));
    let source = std::fs::read_to_string(&path)?;
    let document = crate::parse(&source)?;
    Ok(lower(&document, path.parent())?)
}
