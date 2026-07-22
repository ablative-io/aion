//! BC-5 codegen inspection: the IR-14 calling-convention row (`AWL-BC-IR.md`
//! line 715) and its §11 refinements R5–R8, asserted at the INSTRUCTION level
//! against decoded `select()` output — the direct byte/instruction proof the
//! BC-4 round-3 ruling deferred here (IR-14 is not observable at the durable
//! trail level).
//!
//! Every oracle rests on facts carried INDEPENDENTLY into the decoded check,
//! not restated from the emitted shape (the BC-5 review's core correction):
//! the register-safety facts come from a real CFG dataflow ([`super::inspect_cfg`]);
//! framing/exit/marshaling from the input MIR and the module's import/local
//! target metadata ([`super::inspect_analysis`]); and every inspection routes
//! through the one witnessed decode path so no conclusion rests on a decoded
//! prefix ([`super::inspect_support::decode`]/`decode_bytes`).
//!
//! Two witnesses per claim: a sweep over the full covered ratchet (pinned to
//! `COVERED`, so silent coverage loss fails), and targeted per-shape fixtures.

use beamr::atom::AtomTable;
use beamr::loader::decode::{Instruction, Operand};
use beamr::loader::load::ParsedModule;

use super::inspect_analysis::{
    assert_live_accuracy, check_framed, check_frameless, check_function, expected_framed,
    flow_named, flow_returns, function_arity, is_known, local_target_arities,
};
use super::inspect_support::{
    decode, decode_bytes, destinations, functions, heap_live, instruction_operands, is_call,
    lowered_fixtures, manifest_dir, operand_has_y,
};
use crate::mir::covered::COVERED;
use crate::mir::{
    AtomRef, Block, FlowFn, FnOrigin, FnRef, MirFn, MirModule, RuntimeFn, Span, Stmt, Tail, TyDesc,
    Value, Var, lower, select,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The minimum number of recognised `TestHeap`/`GcBif` heap ops the sweep must
/// inspect — a ratcheted floor (the observed inventory only grows). If fixture
/// or decode coverage drops heap ops, or `heap_live` stops recognising them, the
/// sweep can no longer execute this many `Live` comparisons and fails, so the R8
/// scope cannot silently go vacuous (BC-5 review blocker 8, superseding the
/// uncommitted one-off count).
const MIN_RECOGNISED_HEAP_OPS: usize = 2101;

// ---- the covered-ratchet sweep ----

/// Every module-wide and per-function IR-14/§11 invariant, over every fixture the
/// direct compiler lowers, pinned to the exact `COVERED` ratchet and to a floor
/// on the recognised heap-op inventory. A single failing fixture names itself.
#[test]
fn covered_ratchet_upholds_ir14_and_section_11() -> TestResult {
    let mut fixtures = 0_usize;
    let mut heap_ops = 0_usize;
    for (label, module) in lowered_fixtures()? {
        let (parsed, table) = decode(&module)?;
        module_invariants(&label, &parsed)?;

        let local_arities = local_target_arities(&parsed);
        for function in functions(&parsed, &table) {
            check_function(
                &label,
                &function.name,
                function.code,
                &parsed,
                &table,
                &local_arities,
            )?;
            // R8 scope: count the heap ops actually examined, and cross-check the
            // MIR framing expectation for the flow functions we can name.
            heap_ops += function
                .code
                .iter()
                .filter(|instruction| heap_live(instruction).is_some())
                .count();
            check_mir_framing(&label, &function.name, function.code, &module.functions)?;
        }
        fixtures += 1;
    }
    assert_eq!(
        fixtures,
        COVERED.len(),
        "the sweep inspected {fixtures} fixtures but the pinned ratchet has {} — silent \
         covered→refused drift (BC-5 review advisory B)",
        COVERED.len()
    );
    assert!(
        heap_ops >= MIN_RECOGNISED_HEAP_OPS,
        "the sweep examined only {heap_ops} heap ops, below the ratcheted floor \
         {MIN_RECOGNISED_HEAP_OPS} — the R8 `Live` scope shrank (BC-5 review blocker 8)"
    );
    Ok(())
}

/// The module-wide R6 / register invariants: no `Trim` and no foreign opcode
/// anywhere (closed alphabet); a `Y` register is WRITTEN only by a `move`; and no
/// call carries a `Y` operand (so no value crosses a call from a `Y` home without
/// an `X` reload). The stronger §11.2 "Y read only via `move`" wording is FALSE
/// against these bytes and is STOPPED, not weakened — see §11.9.
fn module_invariants(label: &str, parsed: &ParsedModule) -> TestResult {
    for instruction in &parsed.instructions {
        if !is_known(instruction) {
            return Err(
                format!("{label}: unexpected instruction in BC-3 output: {instruction:?}").into(),
            );
        }
        if matches!(instruction, Instruction::Trim { .. }) {
            return Err(format!("{label}: R6 violated — `trim` was emitted").into());
        }
        if !matches!(instruction, Instruction::Move { .. })
            && destinations(instruction)
                .iter()
                .any(|operand| operand_has_y(operand))
        {
            return Err(format!(
                "{label}: a non-`move` instruction WRITES a Y register: {instruction:?}"
            )
            .into());
        }
        if is_call(instruction)
            && instruction_operands(instruction)
                .iter()
                .any(|operand| operand_has_y(operand))
        {
            return Err(format!(
                "{label}: a call carries a Y operand (value crosses a call without an X reload): \
                 {instruction:?}"
            )
            .into());
        }
    }
    Ok(())
}

/// Cross-checks a decoded function's framing against the INPUT MIR for the flow
/// functions we can name (BC-5 review blocker 5): a function the MIR expects
/// framed (≥1 param or ≥1 def) must carry an `Allocate`, one expected frameless
/// must not, and a function whose MIR body has a non-tail `Return` must carry a
/// `Deallocate; Return` exit. Templated shells (no `FlowFn`) are covered by the
/// byte-level structural checks and skipped here.
fn check_mir_framing(
    label: &str,
    name: &str,
    code: &[Instruction],
    functions: &[MirFn],
) -> TestResult {
    let Some(flow) = flow_named(functions, name) else {
        return Ok(());
    };
    let decoded_framed = code
        .iter()
        .any(|instruction| matches!(instruction, Instruction::Allocate { .. }));
    if expected_framed(flow) != decoded_framed {
        return Err(format!(
            "{label}::{name}: MIR expects framed={} (params/defs) but decoded framed={decoded_framed}",
            expected_framed(flow)
        )
        .into());
    }
    if decoded_framed && flow_returns(flow) {
        let has_exit = code.windows(2).any(|pair| {
            matches!(pair[0], Instruction::Deallocate { .. })
                && matches!(pair[1], Instruction::Return)
        });
        if !has_exit {
            return Err(format!(
                "{label}::{name}: MIR returns a value but the framed function has no \
                 `Deallocate; Return` exit (R7 shared exit removed?)"
            )
            .into());
        }
    }
    Ok(())
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

/// Selects and decodes a hand-built module through the witnessed decode path.
fn decoded_module(
    module: &MirModule,
) -> Result<(ParsedModule, AtomTable), Box<dyn std::error::Error>> {
    decode_bytes(&select(module)?)
}

/// Targeted: a framed function (one param, one def) — the exact independent MIR
/// expectation (framed, one prologue spill `move x0 -> y0`, a `Deallocate;
/// Return` exit) checked against the bytes, not read from them (BC-5 review
/// blocker 5).
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
    let built = module("framed", &["ok"], vec![one]);
    let MirFn::Flow(flow_fn) = &built.functions[0] else {
        return Err("hand-built function is not a flow".into());
    };
    assert!(expected_framed(flow_fn), "MIR expects this shape framed");
    assert!(flow_returns(flow_fn), "MIR expects a non-tail return");

    let (parsed, table) = decoded_module(&built)?;
    let function = functions(&parsed, &table)
        .into_iter()
        .find(|function| function.name == "execute")
        .ok_or("no execute function")?;
    let arity = function_arity(function.code).ok_or("no arity")?;
    assert_eq!(arity, 1, "the hand-built execute takes one param");
    check_framed(function.code, arity, "targeted::framed")?;
    Ok(())
}

/// Targeted: a frameless body (no params, no defs, a tail import over
/// immediates) emits no `Allocate`, no `Y`, and a `call_ext_only` tail, and its
/// register use is independently proven X-safe (BC-5 review blockers 3, 5).
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
    let built = module("frameless", &[], vec![body]);
    let MirFn::Flow(flow_fn) = &built.functions[0] else {
        return Err("hand-built function is not a flow".into());
    };
    assert!(
        !expected_framed(flow_fn),
        "MIR expects this shape frameless"
    );

    let (parsed, table) = decoded_module(&built)?;
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
    check_frameless(function.code, 0, "targeted::frameless")?;
    Ok(())
}

/// Targeted heap ops (BC-5 review blocker 8): a record construction emits a
/// `TestHeap` and an `Increment` emits a `gc_bif2` — both carrying `Live` — whose
/// declared `Live` equals the recomputed live-`X` root high-water. Isolates the
/// R8 accuracy oracle on the two `Live`-bearing op families with named fixtures.
#[test]
fn targeted_heap_ops_declare_accurate_live() -> TestResult {
    // A record built from a param: TestHeap for the tuple's heap need.
    let record = flow(
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
    let built = module("heap_record", &["ok"], vec![record]);
    let (parsed, _) = decoded_module(&built)?;
    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::TestHeap { .. })),
        "a record construction must emit a TestHeap"
    );
    assert_live_accuracy("targeted::heap_record", &parsed.instructions)?;

    // An increment: gc_bif2 erlang:'+' carries Live.
    let counter = flow(
        "execute",
        FnOrigin::Execute,
        &[0],
        vec![Stmt::Increment {
            dst: Var(1),
            src: Var(0),
            span: Span { line: 0, column: 0 },
        }],
        Tail::Return(Value::Var(Var(1))),
    );
    let built = module("heap_incr", &[], vec![counter]);
    let (parsed, _) = decoded_module(&built)?;
    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| heap_live(instruction).is_some()
                && matches!(instruction, Instruction::Bif { .. })),
        "an increment must emit a gc_bif carrying Live"
    );
    assert_live_accuracy("targeted::heap_incr", &parsed.instructions)?;
    Ok(())
}

/// Targeted (BC-5 review blocker 4): a value prepared in `x0` before a
/// `TestHeap` and handed back by the following `Return` must be counted `Live`.
/// The decoded slice `move nil -> x0; TestHeap Live=n; Return` reads `x0` at the
/// `Return` (its ABI result), so the live-`X` root high-water across the heap op
/// is 1: `Live=1` is accurate and `Live=0` — which would let a GC at the heap
/// check clear the term `Return` consumes — is rejected. This pins that the
/// shared read classifier models `Return`'s implicit `x0` read; without it the
/// backward liveness computes an empty set at the heap op and wrongly accepts
/// `Live=0`.
#[test]
fn return_keeps_x0_live_across_a_preceding_heap_op() -> TestResult {
    let prepared = |live: u64| {
        vec![
            Instruction::Move {
                source: Operand::Atom(None),
                destination: Operand::X(0),
            },
            Instruction::TestHeap {
                heap_need: Operand::Unsigned(2),
                live: Operand::Unsigned(live),
            },
            Instruction::Return,
        ]
    };
    // Live=1 counts the x0 that `Return` consumes: accurate.
    assert_live_accuracy("return_live::accurate", &prepared(1))?;
    // Live=0 would let GC clear the returned value: rejected.
    assert!(
        assert_live_accuracy("return_live::cleared", &prepared(0)).is_err(),
        "Live=0 on a heap op before `Return` did not go red — the read classifier \
         does not model `Return`'s implicit x0 read"
    );
    Ok(())
}

/// The oracle-mutation test (BC-5 review blocker 8): a deliberately wrong `Live`
/// makes the R8 accuracy assertion go RED. Take a real decoded heap op, bump its
/// declared `Live` by one, and prove `assert_live_accuracy` now errs — the
/// equality oracle is mutation-sensitive, not vacuous.
#[test]
fn wrong_live_expectation_goes_red() -> TestResult {
    let (parsed, _) = decode(&fixture_module(
        "loop-outcomes/valid/loop_counting_until_max",
    )?)?;
    let mut code = parsed.instructions.clone();
    let (at, _) = code
        .iter()
        .enumerate()
        .find(|(_, instruction)| heap_live(instruction).is_some())
        .ok_or("the loop fixture emitted no heap op to mutate")?;
    // The un-mutated stream is accurate.
    assert_live_accuracy("mutation::baseline", &code)?;
    // Bump exactly one heap op's declared Live: the oracle must catch it.
    code[at] = bump_live(&code[at]).ok_or("could not bump the heap op's Live")?;
    assert!(
        assert_live_accuracy("mutation::bumped", &code).is_err(),
        "a wrong Live expectation did not go red — the R8 oracle is vacuous"
    );
    Ok(())
}

/// Returns a copy of a `TestHeap`/`GcBif` heap op with its declared `Live`
/// incremented by one (for the oracle-mutation test), or `None` when it is not a
/// recognised heap op.
fn bump_live(instruction: &Instruction) -> Option<Instruction> {
    match instruction {
        Instruction::TestHeap { heap_need, live } => Some(Instruction::TestHeap {
            heap_need: heap_need.clone(),
            live: bump_unsigned(live)?,
        }),
        Instruction::Bif { op, operands } => {
            let mut operands = operands.clone();
            let bumped = bump_unsigned(operands.get(1)?)?;
            operands[1] = bumped;
            Some(Instruction::Bif { op: *op, operands })
        }
        _ => None,
    }
}

/// Increments an `Operand::Unsigned` by one.
fn bump_unsigned(operand: &Operand) -> Option<Operand> {
    match operand {
        Operand::Unsigned(value) => Some(Operand::Unsigned(value + 1)),
        _ => None,
    }
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

/// Targeted route-to-step tail call (BC-5 review blocker 7): a route to another
/// STEP lowers to a region-to-region LOCAL tail call. In
/// `sequence_region_loopback` (`route push` / `route settle`), some function ends
/// its transfer in a `call_last` / `call_only` targeting ANOTHER decoded
/// function's own body label — a real local callee resolved through the module's
/// `FuncInfo` table, not an unrelated shell `call_ext_last`. A regression from a
/// tail route to `call … ; return` would remove that local tail.
#[test]
fn targeted_route_to_step_is_a_local_tail_call() -> TestResult {
    let (parsed, table) = decode(&fixture_module(
        "flow-shape/valid/sequence_region_loopback",
    )?)?;
    let local_arities = local_target_arities(&parsed);
    let mut found = false;
    for function in functions(&parsed, &table) {
        let own_body = match function.code.get(2) {
            Some(Instruction::Label { label }) => Some(*label),
            _ => None,
        };
        for instruction in function.code {
            let (Instruction::CallLast { label, .. } | Instruction::CallOnly { label, .. }) =
                instruction
            else {
                continue;
            };
            // A LOCAL tail call to ANOTHER function's body label is a
            // region-to-region route-to-step (self targets are loop back-edges).
            if let Operand::Label(target) = label
                && local_arities.contains_key(target)
                && Some(*target) != own_body
            {
                found = true;
            }
        }
    }
    assert!(
        found,
        "no region-to-region local tail call — a route-to-step is not a tail call"
    );
    Ok(())
}

/// DIVERGENCE surfaced, not papered over (BC-5 review blocker 7): a
/// SUCCESS-OUTCOME route (`awl_hello`'s `route shouted`) does NOT lower to a tail
/// call — `route_tail` (`lower/route.rs:60-71`) builds `Ok(Shouted(payload))` and
/// RETURNS it (`Tail::Return`). The old targeted test asserted "`awl_hello` emits
/// a `call_ext_last` for its route", which any unrelated shell/decoder tail
/// satisfied — the exact weakness the review flagged, resting on a false premise.
/// The
/// tail-call claim for routes is proven where it holds (route-to-STEP in
/// `targeted_route_to_step_is_a_local_tail_call`; loop recursion in
/// `targeted_loop_recursion_is_a_self_tail_call`). Here the outcome-route shape is
/// pinned as it truly is: a framed function returns the `{ok, {shouted, …}}`
/// term through a `Deallocate; Return` exit — not a route tail call.
#[test]
fn awl_hello_outcome_route_returns_not_tail_calls() -> TestResult {
    let (parsed, table) = decode(&fixture_module("flagship/valid/awl_hello")?)?;
    let outcome_return = functions(&parsed, &table).iter().any(|function| {
        function.code.windows(2).any(|pair| {
            matches!(pair[0], Instruction::Deallocate { .. })
                && matches!(pair[1], Instruction::Return)
        })
    });
    assert!(
        outcome_return,
        "awl_hello has no framed `Deallocate; Return` — its success-outcome route must Return, \
         not tail-call a `success` runtime function"
    );
    Ok(())
}

/// Lowers a covered fixture by its `<dir>/<stem>` label to a `MirModule`.
fn fixture_module(relative: &str) -> Result<MirModule, Box<dyn std::error::Error>> {
    let path = manifest_dir()
        .join("tests/fixtures/rev2")
        .join(format!("{relative}.awl"));
    let source = std::fs::read_to_string(&path)?;
    let document = crate::parse(&source)?;
    Ok(lower(&document, path.parent())?)
}
