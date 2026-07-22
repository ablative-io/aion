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

use std::collections::{BTreeMap, BTreeSet};

use beamr::atom::AtomTable;
use beamr::loader::decode::{Instruction, Operand};
use beamr::loader::load::ParsedModule;

use super::assemble::lower_bodies;
use super::emit::frame_homes;
use super::inspect_analysis::{
    assert_live_accuracy, check_framed, check_frameless, check_function, expected_framed,
    flow_named, flow_returns, function_arity, is_known, local_target_arities,
};
use super::inspect_cfg::{reachable_from_entry, reachable_without_tail, x_safety_violations};
use super::inspect_expect::check_marshaling;
use super::inspect_support::{
    decode, decode_bytes, destinations, functions, heap_live, instruction_operands, is_call,
    lowered_fixtures, make_fun_num_free, manifest_dir, name_of, operand_has_y,
    with_explicit_make_fun_reads,
};
use super::ir::{Body, TailKind};
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

        // The selected bodies carry the marshaling expectations (arg sources,
        // arities, result stores) INDEPENDENTLY into the decoded check (blocker 6).
        let (bodies, emit_atoms) = lower_bodies(&module)?;
        let expected: BTreeMap<String, &Body> = bodies
            .iter()
            .map(|body| (name_of(&emit_atoms, body.name), body))
            .collect();

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
            let body = expected.get(function.name.as_str()).ok_or_else(|| {
                format!(
                    "{label}::{}: no selected body matches the decoded name",
                    function.name
                )
            })?;
            let homes = frame_homes(body)?;
            check_marshaling(
                &format!("{label}::{}", function.name),
                function.code,
                body,
                &homes,
                &parsed,
                &table,
                &emit_atoms,
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

/// Focused oracle (BC-5 review blocker 3): a real closure's implicit captures
/// `x0..x(num_free-1)` must be visible to the register-safety analysis. The raw
/// decoded `make_fun2` carries only its `FunT` index, so its capture reads are
/// invisible until [`with_explicit_make_fun_reads`] resolves `num_free` and makes
/// them explicit. Scan the covered fixtures for a clean witness — a closure whose
/// top capture register is defined ONLY by the marshal reload right before the
/// `make_fun2` — then prove: the emitted closure is X-safe; deleting that reload
/// is INVISIBLE to the raw analysis (the hole); and it is CAUGHT once the implicit
/// reads are explicit (the fix).
#[test]
fn make_fun_capture_reads_are_modeled_and_a_removed_reload_is_caught() -> TestResult {
    for (_label, module) in lowered_fixtures()? {
        let (parsed, table) = decode(&module)?;
        for function in functions(&parsed, &table) {
            let Some(at) = function
                .code
                .iter()
                .position(|instruction| matches!(instruction, Instruction::MakeFun { .. }))
            else {
                continue;
            };
            let Some(num_free) = make_fun_num_free(&parsed.lambdas, &function.code[at])? else {
                continue;
            };
            if num_free == 0 {
                continue;
            }
            // The emitted closure must be register-safe once captures are explicit.
            if !x_safety_violations(&with_explicit_make_fun_reads(
                function.code,
                &parsed.lambdas,
            )?)
            .is_empty()
            {
                continue;
            }
            // Try each capture register: a clean witness is one whose marshal reload
            // is the SOLE definition reaching the `make_fun2`, so deleting it is
            // invisible to the raw one-operand op (its captures are unmodeled) yet
            // undefined-at-`make_fun2` once the implicit reads are explicit. A
            // register the emitter happens to define elsewhere (a stale value) is
            // the source problem (blocker 6), not this definedness problem, so it is
            // skipped here.
            for register in (0..num_free).rev() {
                let Some(reload) = marshal_reload_of(function.code, at, register) else {
                    continue;
                };
                let mut mutated = function.code.to_vec();
                mutated.remove(reload);
                let invisible_raw = x_safety_violations(&mutated).is_empty();
                let caught_explicit =
                    !x_safety_violations(&with_explicit_make_fun_reads(&mutated, &parsed.lambdas)?)
                        .is_empty();
                if invisible_raw && caught_explicit {
                    // Witness found: the deletion is invisible to the raw op but
                    // caught once `num_free` makes the capture reads explicit —
                    // proving the implicit captures are modeled (blocker 3).
                    return Ok(());
                }
            }
        }
    }
    Err(
        "no covered closure has a capture register whose reload deletion is invisible to the raw \
         make_fun2 yet caught once implicit captures are explicit — blocker 3 is unexercised"
            .into(),
    )
}

/// The index of the marshal `move <src> -> x(register)` that loads a capture into
/// `register` for the `make_fun2` at `make_fun`, searching the marshal block back
/// to the previous call/label. `None` when no such reload is found.
fn marshal_reload_of(code: &[Instruction], make_fun: usize, register: u32) -> Option<usize> {
    for index in (0..make_fun).rev() {
        match code.get(index)? {
            Instruction::Move {
                destination: Operand::X(reg),
                ..
            } if *reg == register => return Some(index),
            instruction
                if is_call(instruction) || matches!(instruction, Instruction::Label { .. }) =>
            {
                return None;
            }
            _ => {}
        }
    }
    None
}

/// The nearest `move <src> -> x(register)` marshal reaching the call at `call`.
fn marshal_move(code: &[Instruction], call: usize, register: u32) -> Option<usize> {
    (0..call).rev().find(|&index| {
        matches!(code.get(index), Some(Instruction::Move { destination: Operand::X(r), .. }) if *r == register)
    })
}

/// The `source` operand of a `move` at `index`.
fn move_source(code: &[Instruction], index: usize) -> Option<Operand> {
    match code.get(index) {
        Some(Instruction::Move { source, .. }) => Some(source.clone()),
        _ => None,
    }
}

/// Targeted marshaling oracle (BC-5 review blocker 6): the marshaling check must
/// reject a SWAPPED argument, a DROPPED result store, and a WRONG arity — not just
/// "some write happened". `execute(a, b) { let r = a <> b; return r }` marshals
/// `move y0 -> x0; move y1 -> x1; call_ext append/2; move x0 -> y2`. Each mutation
/// of the real decoded bytes must go red while the unmutated stream passes.
#[test]
fn marshaling_rejects_swapped_args_dropped_store_and_wrong_arity() -> TestResult {
    let concat = flow(
        "execute",
        FnOrigin::Execute,
        &[0, 1],
        vec![Stmt::Concat {
            dst: Var(2),
            lhs: Value::Var(Var(0)),
            rhs: Value::Var(Var(1)),
            span: Span { line: 0, column: 0 },
        }],
        Tail::Return(Value::Var(Var(2))),
    );
    let built = module("concat2", &[], vec![concat]);
    let (bodies, emit_atoms) = lower_bodies(&built)?;
    let body = bodies
        .iter()
        .find(|body| name_of(&emit_atoms, body.name) == "execute")
        .ok_or("no execute body")?;
    let homes = frame_homes(body)?;
    let (parsed, table) = decoded_module(&built)?;
    let function = functions(&parsed, &table)
        .into_iter()
        .find(|function| function.name == "execute")
        .ok_or("no execute function")?;
    let where_ = "concat2::execute";
    let run = |code: &[Instruction]| {
        check_marshaling(where_, code, body, &homes, &parsed, &table, &emit_atoms)
    };
    // Baseline: the real bytes marshal a into x0, b into x1, and store the result.
    run(function.code)?;

    let call = function
        .code
        .iter()
        .position(|instruction| matches!(instruction, Instruction::CallExt { .. }))
        .ok_or("no call_ext append")?;
    let x0 = marshal_move(function.code, call, 0).ok_or("no x0 marshal")?;
    let x1 = marshal_move(function.code, call, 1).ok_or("no x1 marshal")?;
    let s0 = move_source(function.code, x0).ok_or("x0 marshal is not a move")?;
    let s1 = move_source(function.code, x1).ok_or("x1 marshal is not a move")?;

    // (1) Swap the two argument sources: x0 now carries b, x1 carries a.
    let mut swapped = function.code.to_vec();
    swapped[x0] = Instruction::Move {
        source: s1,
        destination: Operand::X(0),
    };
    swapped[x1] = Instruction::Move {
        source: s0,
        destination: Operand::X(1),
    };
    assert!(
        run(&swapped).is_err(),
        "a swapped argument marshal (y1 -> x0 / y0 -> x1) was accepted — source identity unchecked"
    );

    // (2) Drop the result store immediately after the call.
    let mut no_store = function.code.to_vec();
    no_store.remove(call + 1);
    assert!(
        run(&no_store).is_err(),
        "a dropped result store was accepted — the store is not required unconditionally"
    );

    // (3) Declare the wrong call arity.
    let mut wrong_arity = function.code.to_vec();
    if let Instruction::CallExt { import, .. } = &function.code[call] {
        wrong_arity[call] = Instruction::CallExt {
            arity: Operand::Unsigned(1),
            import: import.clone(),
        };
    }
    assert!(
        run(&wrong_arity).is_err(),
        "a wrong declared arity was accepted — arity is not compared to the selected step"
    );
    Ok(())
}

/// The shared-exit label of a framed function: the `Label` immediately preceding
/// its single standalone `Deallocate` (the `Lexit: Deallocate F; Return` block).
fn exit_label(code: &[Instruction]) -> Option<u32> {
    let dealloc = code
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Deallocate { .. }))?;
    match code.get(dealloc.checked_sub(1)?) {
        Some(Instruction::Label { label }) => Some(*label),
        _ => None,
    }
}

/// Asserts that on EVERY edge to the shared exit (`jump exit`), the returned
/// value is reloaded into `x0` immediately beforehand — `move <src> -> x0` — and,
/// when `expected_home` is given, that `<src>` is exactly the return var's frame
/// home `Y(expected_home)`. Pins the selector's `reload(return_src, 0); jump
/// Lexit` shape with SOURCE identity (BC-5 review blocker 5), so a reload
/// redirected off `x0` or sourced from the wrong home is rejected.
fn assert_return_reload_into_x0(
    code: &[Instruction],
    exit: u32,
    expected_home: Option<u32>,
) -> TestResult {
    let mut edges = 0_usize;
    for (index, instruction) in code.iter().enumerate() {
        if !matches!(instruction, Instruction::Jump { target: Operand::Label(t) } if *t == exit) {
            continue;
        }
        edges += 1;
        let prior = index.checked_sub(1).and_then(|p| code.get(p));
        let Some(Instruction::Move {
            source,
            destination: Operand::X(0),
        }) = prior
        else {
            return Err(format!(
                "the jump to the shared exit at {index} is not preceded by a `move <src> -> x0` \
                 return reload (got {prior:?})"
            )
            .into());
        };
        if let Some(home) = expected_home
            && !matches!(source, Operand::Y(h) if *h == home)
        {
            return Err(format!(
                "the return reload before the exit jump sources {source:?}, not the return var's \
                 home y{home}"
            )
            .into());
        }
    }
    if edges == 0 {
        return Err("no jump to the shared exit — cannot pin the return reload".into());
    }
    Ok(())
}

/// Targeted (BC-5 review blocker 5): the value a framed function returns is
/// reloaded into `x0` on the edge to the shared exit, sourced from the return
/// var's own frame home. `execute(x0)` builds a record into `Var(1)` (homed at
/// `y1`) and returns it, so the exit edge is `move y1 -> x0; jump Lexit`. Redirect
/// that reload to `x1` — the exact emitter regression the review names — and the
/// oracle must reject it, proving source identity is pinned, not just "some x0".
#[test]
fn framed_return_reloads_the_return_value_into_x0() -> TestResult {
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
    let built = module("framed_return", &["ok"], vec![one]);
    let (parsed, table) = decoded_module(&built)?;
    let function = functions(&parsed, &table)
        .into_iter()
        .find(|function| function.name == "execute")
        .ok_or("no execute function")?;
    let exit = exit_label(function.code).ok_or("framed function has no shared exit label")?;
    // `Var(1)` is the sole def, homed at `y1` (params first, then defs in order).
    assert_return_reload_into_x0(function.code, exit, Some(1))?;

    // Redirect the return reload away from x0 (move y1 -> x1): must be rejected.
    let mut mutated = function.code.to_vec();
    let jump = mutated
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Jump { target: Operand::Label(t) } if *t == exit))
        .ok_or("no exit jump to mutate")?;
    let reload = jump.checked_sub(1).ok_or("exit jump has no predecessor")?;
    if let Some(Instruction::Move { source, .. }) = mutated.get(reload) {
        mutated[reload] = Instruction::Move {
            source: source.clone(),
            destination: Operand::X(1),
        };
    }
    assert!(
        assert_return_reload_into_x0(&mutated, exit, Some(1)).is_err(),
        "a return reload redirected off x0 was accepted — the exit reload's source identity is \
         not pinned"
    );
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

/// The body label of a decoded function (the `Label` after its `FuncInfo`, the
/// target a local call transfers to).
fn body_label(code: &[Instruction]) -> Option<u32> {
    match code.get(2) {
        Some(Instruction::Label { label }) => Some(*label),
        _ => None,
    }
}

/// Every local-body label a decoded function tail-transfers to (a `call_last` /
/// `call_only` whose label resolves in the module's `FuncInfo` table).
fn local_tail_targets(code: &[Instruction], local_arities: &BTreeMap<u32, u32>) -> BTreeSet<u32> {
    code.iter()
        .filter_map(|instruction| match instruction {
            Instruction::CallLast {
                label: Operand::Label(target),
                ..
            }
            | Instruction::CallOnly {
                label: Operand::Label(target),
                ..
            } if local_arities.contains_key(target) => Some(*target),
            _ => None,
        })
        .collect()
}

/// The index of a framed function's shared exit `Deallocate` (the one
/// immediately followed by `Return`).
fn shared_exit_dealloc(code: &[Instruction]) -> Option<usize> {
    code.windows(2).position(|pair| {
        matches!(pair[0], Instruction::Deallocate { .. }) && matches!(pair[1], Instruction::Return)
    })
}

/// The selected body whose function-name atom resolves to `name`.
fn body_named<'b>(bodies: &'b [Body], emit_atoms: &AtomTable, name: &str) -> Option<&'b Body> {
    bodies
        .iter()
        .find(|body| name_of(emit_atoms, body.name) == name)
}

/// Whether a selected tail reaches a `Return` on any branch arm.
fn tail_returns(tail: &TailKind) -> bool {
    match tail {
        TailKind::Return(_) => true,
        TailKind::If {
            then_block,
            else_block,
            ..
        } => tail_returns(&then_block.tail) || tail_returns(&else_block.tail),
        TailKind::SelectEnum { arms, .. } => {
            arms.iter().any(|(_, block)| tail_returns(&block.tail))
        }
        _ => false,
    }
}

/// Whether a selected tail reaches a LOCAL tail call on any branch arm.
fn tail_has_local_tail(tail: &TailKind) -> bool {
    match tail {
        TailKind::TailLocal { .. } => true,
        TailKind::If {
            then_block,
            else_block,
            ..
        } => tail_has_local_tail(&then_block.tail) || tail_has_local_tail(&else_block.tail),
        TailKind::SelectEnum { arms, .. } => arms
            .iter()
            .any(|(_, block)| tail_has_local_tail(&block.tail)),
        _ => false,
    }
}

/// Whether a selected tail reaches ANY tail call (local or external) on any arm.
fn tail_has_any_tail_call(tail: &TailKind) -> bool {
    match tail {
        TailKind::TailLocal { .. } | TailKind::TailImport { .. } => true,
        TailKind::If {
            then_block,
            else_block,
            ..
        } => tail_has_any_tail_call(&then_block.tail) || tail_has_any_tail_call(&else_block.tail),
        TailKind::SelectEnum { arms, .. } => arms
            .iter()
            .any(|(_, block)| tail_has_any_tail_call(&block.tail)),
        TailKind::Return(_) => false,
    }
}

/// Proves a decoded function's Return-route arm(s) genuinely RETURN (BC-5 review
/// blocker 7): no external tail transfer (`call_ext_last`/`call_ext_only`)
/// anywhere; no local tail (`call_last`/`call_only`) either unless
/// `allow_local_tail` (the one legitimate route-to-step tail, e.g. `route push`);
/// and the shared `Deallocate; Return` exit is reachable from entry by a path
/// carrying NO tail transfer at all. A Return arm regressed to a local or external
/// tail removes the only edge to the exit and/or introduces a forbidden tail, so
/// the check goes red.
fn return_route_reaches_exit(code: &[Instruction], allow_local_tail: bool) -> TestResult {
    if code.iter().any(|instruction| {
        matches!(
            instruction,
            Instruction::CallExtLast { .. } | Instruction::CallExtOnly { .. }
        )
    }) {
        return Err("a Return-route function carries an external tail transfer".into());
    }
    if !allow_local_tail
        && code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::CallLast { .. } | Instruction::CallOnly { .. }
            )
        })
    {
        return Err("a pure-return function carries a local tail transfer".into());
    }
    let exit = shared_exit_dealloc(code).ok_or("no shared `Deallocate; Return` exit")?;
    if !reachable_without_tail(code).contains(&exit) {
        return Err(
            "the shared `Deallocate; Return` exit is not reachable by a no-tail path — the \
             Return route was replaced by a tail transfer"
                .into(),
        );
    }
    Ok(())
}

/// Replaces the first `jump exit` (a Return arm's transfer to the shared exit)
/// with an external tail call — the mutation the Return-route oracle must reject.
fn replace_exit_jump_with_external_tail(
    code: &[Instruction],
    exit: u32,
) -> Option<Vec<Instruction>> {
    let at = code.iter().position(
        |instruction| matches!(instruction, Instruction::Jump { target: Operand::Label(t) } if *t == exit),
    )?;
    let mut out = code.to_vec();
    out[at] = Instruction::CallExtLast {
        arity: Operand::Unsigned(0),
        import: Operand::Unsigned(0),
        deallocate: Operand::Unsigned(0),
    };
    Some(out)
}

/// Targeted route-to-step tail call (BC-5 review blocker 7): the SPECIFIC caller →
/// target edge in `sequence_region_loopback`. The `confirm` step has two outcomes
/// — `retry` (`route push`) and `move_on` (`route settle`). `route push` re-enters
/// the region member `push`, so `step_confirm` LOCAL-TAIL-CALLS `step_push`'s own
/// body label, on a live branch from entry, with no call-plus-return on that edge.
/// `route settle` targets the region's `collect`/exit step, which `route_tail`
/// resolves to `Ok(<collected>)` and RETURNS (`lower/route.rs` exit handling) — so
/// there is deliberately NO `step_settle` function and NO second step tail call.
/// The oracle pins the EXACT set of `step_confirm`'s local-tail targets to
/// `{step_push}`: a regression that turned `route push` into a call-plus-return
/// would empty the set, and one that turned `route settle` (a region exit) into a
/// tail call would add a second target — either fails. This replaces the earlier
/// existential search that any unrelated chain fall-through satisfied.
#[test]
fn targeted_route_to_step_is_a_local_tail_call() -> TestResult {
    let module = fixture_module("flow-shape/valid/sequence_region_loopback")?;
    let (parsed, table) = decode(&module)?;
    let local_arities = local_target_arities(&parsed);
    let decoded = functions(&parsed, &table);
    let find = |name: &str| decoded.iter().find(|function| function.name == name);

    let confirm = find("awl_r0_ordered_step_confirm")
        .ok_or("no awl_r0_ordered_step_confirm function decoded")?;
    let push_body = find("awl_r0_ordered_step_push")
        .and_then(|function| body_label(function.code))
        .ok_or("no awl_r0_ordered_step_push body label")?;

    // `route push` is a local tail call to step_push's body, reachable from entry.
    let reachable = reachable_from_entry(confirm.code);
    let push_tail = confirm
        .code
        .iter()
        .position(|instruction| {
            matches!(instruction,
                Instruction::CallLast { label: Operand::Label(t), .. }
                | Instruction::CallOnly { label: Operand::Label(t), .. } if *t == push_body)
        })
        .ok_or("step_confirm has no local tail call to step_push (route push)")?;
    if !reachable.contains(&push_tail) {
        return Err("step_confirm's tail call to step_push is unreachable from entry".into());
    }

    // Absence of call-plus-return on the push route edge: no NON-tail `call`
    // targets step_push (a regressed `call step_push; … ; return` would).
    let push_call_return = confirm.code.iter().any(|instruction| {
        matches!(instruction, Instruction::Call { label: Operand::Label(t), .. } if *t == push_body)
    });
    if push_call_return {
        return Err(
            "step_confirm reaches step_push by a non-tail call-plus-return, not a route \
             tail call"
                .into(),
        );
    }

    // The EXACT local-tail-target set is {step_push}: route push tail-calls, and
    // route settle (the region collect exit) returns — it must NOT tail-call.
    let targets = local_tail_targets(confirm.code, &local_arities);
    let expected: BTreeSet<u32> = std::iter::once(push_body).collect();
    if targets != expected {
        return Err(format!(
            "step_confirm's local-tail targets are {targets:?}, expected exactly {{step_push={push_body}}} — \
             route settle (a region collect exit) must Return, not tail-call"
        )
        .into());
    }

    // The settle arm (`route settle` = the region collect exit) genuinely RETURNS,
    // not merely "is not a LOCAL tail": no external tail transfer anywhere, and the
    // shared `Deallocate; Return` exit is reachable by a no-tail path. Independently
    // anchored on the selected body, whose `confirm` tail carries both a Return arm
    // (settle) and a `TailLocal` arm (push).
    let (bodies, emit_atoms) = lower_bodies(&module)?;
    let confirm_body = body_named(&bodies, &emit_atoms, "awl_r0_ordered_step_confirm")
        .ok_or("no selected body for step_confirm")?;
    if !tail_returns(&confirm_body.tail) || !tail_has_local_tail(&confirm_body.tail) {
        return Err(
            "the selected step_confirm tail lacks the expected Return (settle) + TailLocal (push) \
             arms"
                .into(),
        );
    }
    return_route_reaches_exit(confirm.code, true)?;

    // Mutation: turn the settle Return arm's transfer-to-exit into an external tail
    // — the exact regression the review named. It must be rejected.
    let exit = exit_label(confirm.code).ok_or("no shared exit label for step_confirm")?;
    let mutated = replace_exit_jump_with_external_tail(confirm.code, exit)
        .ok_or("no exit jump in step_confirm to mutate")?;
    if return_route_reaches_exit(&mutated, true).is_ok() {
        return Err(
            "a settle arm regressed to an external tail was accepted — the Return route is not \
             pinned"
                .into(),
        );
    }
    Ok(())
}

/// DIVERGENCE surfaced, not papered over (BC-5 review blocker 7): a
/// SUCCESS-OUTCOME route (`awl_hello`'s `route shouted`) does NOT lower to a tail
/// call — `route_tail` (`lower/route.rs:60-71`) builds `Ok(Shouted(payload))` and
/// RETURNS it (`Tail::Return`). Rather than the old `.any()` over every decoded
/// function (which countless unrelated codec/helper `Deallocate; Return` pairs
/// satisfy), this selects `step_greet_and_shout` — the function carrying the
/// `route shouted` arm — and proves THAT arm returns: independently anchored on its
/// selected tail (a `Return` route with no tail-call arm), it carries no tail
/// transfer and reaches its shared `Deallocate; Return` exit by a no-tail path.
/// A mutation turning the shouted arm into an external tail is rejected. The
/// tail-call claim for routes is proven where it holds (route-to-STEP in
/// `targeted_route_to_step_is_a_local_tail_call`; loop recursion in
/// `targeted_loop_recursion_is_a_self_tail_call`).
#[test]
fn awl_hello_outcome_route_returns_not_tail_calls() -> TestResult {
    let module = fixture_module("flagship/valid/awl_hello")?;
    let (parsed, table) = decode(&module)?;
    let step = functions(&parsed, &table)
        .into_iter()
        .find(|function| function.name == "step_greet_and_shout")
        .ok_or("no step_greet_and_shout function decoded")?;

    // Independent anchor: the selected `step_greet_and_shout` tail is a `Return`
    // route (route shouted), with no tail-call arm — the success-outcome divergence.
    let (bodies, emit_atoms) = lower_bodies(&module)?;
    let body = body_named(&bodies, &emit_atoms, "step_greet_and_shout")
        .ok_or("no selected body for step_greet_and_shout")?;
    if !tail_returns(&body.tail) || tail_has_any_tail_call(&body.tail) {
        return Err("the selected step_greet_and_shout tail is not a pure Return route".into());
    }

    // Decoded: the shouted arm reaches the shared `Deallocate; Return` exit with NO
    // tail transfer (local or external).
    return_route_reaches_exit(step.code, false)?;

    // Mutation: turn the shouted Return arm's transfer-to-exit into an external tail
    // — must be rejected.
    let exit = exit_label(step.code).ok_or("no shared exit label for step_greet_and_shout")?;
    let mutated = replace_exit_jump_with_external_tail(step.code, exit)
        .ok_or("no exit jump in step_greet_and_shout to mutate")?;
    if return_route_reaches_exit(&mutated, false).is_ok() {
        return Err(
            "a shouted route arm regressed to an external tail was accepted — the Return route is \
             not pinned"
                .into(),
        );
    }
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
