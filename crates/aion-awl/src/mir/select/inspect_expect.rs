//! IR-14 marshaling oracle for the BC-5 codegen inspection (BC-5 review
//! blocker 6). Where [`super::inspect_analysis::check_call_convention`] checks a
//! call's arity against its TARGET metadata, this module checks the ARGUMENTS:
//! it carries the selected-IR [`Body`] (the emitter's own input) INDEPENDENTLY
//! into the decoded stream and proves, per call, that
//!
//! * every argument register `x0..x(k-1)` (a `call_fun` also the fun in `x(k)`,
//!   a `make_fun2` its captures `x0..x(free-1)`) is marshaled from the EXACT
//!   source the selected step names — the var's frame home (`emit::frame_homes`)
//!   or the immediate — so a swapped or stale `move` is rejected, not merely
//!   "some write happened";
//! * every call's arity equals the selected step's argument count — including
//!   `call_fun`, whose target arity no import/local table declares; and
//! * every value-producing call stores its `x0` result into the destination's
//!   frame home UNCONDITIONALLY (`move x0 -> y(dst_home)` as the very next
//!   instruction), so a dropped result store is caught.
//!
//! Expectations are walked in emission order (steps then tail, branch arms in
//! order) and matched 1:1 against the decoded `is_call` stream; a count or kind
//! mismatch is itself a failure.

use std::collections::HashMap;

use beamr::atom::{Atom, AtomTable};
use beamr::loader::decode::{Instruction, Operand};
use beamr::loader::load::ParsedModule;

use super::inspect_support::{
    as_unsigned, is_call, make_fun_num_free, name_of, operand_index, reads_writes,
};
use super::ir::{Body, Src, Step, TailKind, Via};
use crate::mir::Var;

type CheckResult = Result<(), Box<dyn std::error::Error>>;

/// The expected source of one marshaled argument register.
enum ArgSrc {
    /// A var reloaded from its frame home `Y(home)`.
    Home(u32),
    /// A literal-pool immediate.
    Lit(usize),
    /// An integer immediate.
    Int(i64),
    /// An interned atom immediate (compared by NAME across the two tables).
    Atom(Atom),
    /// The `nil`/`[]` atom (`Operand::Atom(None)`).
    Nil,
    /// A value already in the register in place (the `json.object` argument
    /// assembled by `put_list`), which carries no marshal `move`.
    InPlace,
}

/// The decoded instruction shape one expected call must take.
enum ExpectKind {
    /// `call_ext import` (a non-tail external call).
    Import(usize),
    /// `call label` (a non-tail local call).
    Local(u32),
    /// `call_fun` (the fun reloaded into `x(k)`).
    Fun(ArgSrc),
    /// `make_fun2` (captures in `x0..x(free-1)`).
    Closure,
    /// `call_ext_last` / `call_ext_only`.
    TailImport(usize),
    /// `call_last` / `call_only`.
    TailLocal(u32),
}

/// One expected call in emission order.
struct CallExpect {
    kind: ExpectKind,
    /// Sources for `x0..x(args.len()-1)`.
    args: Vec<ArgSrc>,
    /// The `Y` home the result is stored into, when the call produces a value.
    produces: Option<u32>,
}

/// The decode-side context an expectation is matched against: the parsed module
/// (for `FunT` capture counts), the decode-side atom table, and the emit-side
/// atom table (for resolving an expected atom source's name).
struct Decoded<'a> {
    parsed: &'a ParsedModule,
    table: &'a AtomTable,
    emit_atoms: &'a AtomTable,
}

/// Proves every decoded call in `code` marshals the exact arguments the selected
/// `body` names and stores its result (BC-5 review blocker 6). `emit_atoms`
/// resolves an expected atom source; `table` resolves the decoded operand.
pub(super) fn check_marshaling(
    where_: &str,
    code: &[Instruction],
    body: &Body,
    homes: &HashMap<Var, u32>,
    parsed: &ParsedModule,
    table: &AtomTable,
    emit_atoms: &AtomTable,
) -> CheckResult {
    let mut expects = Vec::new();
    collect_block(&body.steps, &body.tail, homes, &mut expects)?;
    let ctx = Decoded {
        parsed,
        table,
        emit_atoms,
    };

    let mut next = expects.iter();
    for (index, instruction) in code.iter().enumerate() {
        if !is_call(instruction) {
            continue;
        }
        let expect = next.next().ok_or_else(|| {
            format!(
                "{where_}: decoded call {instruction:?} has no matching selected-step call — \
                     more calls emitted than the MIR expects"
            )
        })?;
        check_call(where_, code, index, instruction, expect, &ctx)?;
    }
    if next.next().is_some() {
        return Err(format!(
            "{where_}: fewer decoded calls than the MIR expects — a call/marshal was dropped"
        )
        .into());
    }
    Ok(())
}

/// Matches one decoded call against its expectation: kind, arity, argument
/// sources (and the `call_fun` fun register), and the mandatory result store.
fn check_call(
    where_: &str,
    code: &[Instruction],
    index: usize,
    instruction: &Instruction,
    expect: &CallExpect,
    ctx: &Decoded<'_>,
) -> CheckResult {
    let arity = u32::try_from(expect.args.len()).unwrap_or(u32::MAX);
    match (&expect.kind, instruction) {
        (
            ExpectKind::Import(import),
            Instruction::CallExt {
                arity: a,
                import: op,
            },
        )
        | (
            ExpectKind::TailImport(import),
            Instruction::CallExtLast {
                arity: a,
                import: op,
                ..
            }
            | Instruction::CallExtOnly {
                arity: a,
                import: op,
            },
        ) => {
            require_arity(where_, a, arity, instruction)?;
            require_target(where_, op, *import, "import", instruction)?;
        }
        (
            ExpectKind::Local(label),
            Instruction::Call {
                arity: a,
                label: op,
            },
        )
        | (
            ExpectKind::TailLocal(label),
            Instruction::CallLast {
                arity: a,
                label: op,
                ..
            }
            | Instruction::CallOnly {
                arity: a,
                label: op,
            },
        ) => {
            require_arity(where_, a, arity, instruction)?;
            require_label(where_, op, *label, instruction)?;
        }
        (ExpectKind::Fun(fun), Instruction::CallFun { arity: a }) => {
            require_arity(where_, a, arity, instruction)?;
            // The fun is reloaded into x(arity), after the argument registers.
            check_arg(where_, code, index, arity, fun, ctx)?;
        }
        (ExpectKind::Closure, Instruction::MakeFun { .. }) => {
            let free = make_fun_num_free(&ctx.parsed.lambdas, instruction)?
                .ok_or_else(|| format!("{where_}: make_fun2 lost its capture count"))?;
            if free != arity {
                return Err(format!(
                    "{where_}: make_fun2 declares {free} captures but the selected closure has \
                     {arity}: {instruction:?}"
                )
                .into());
            }
        }
        _ => {
            return Err(format!(
                "{where_}: decoded {instruction:?} does not match the expected call kind"
            )
            .into());
        }
    }

    for (position, arg) in expect.args.iter().enumerate() {
        let register = u32::try_from(position).unwrap_or(u32::MAX);
        check_arg(where_, code, index, register, arg, ctx)?;
    }

    if let Some(home) = expect.produces {
        let store = code.get(index + 1);
        let ok = matches!(
            store,
            Some(Instruction::Move { source: Operand::X(0), destination: Operand::Y(y) }) if *y == home
        );
        if !ok {
            return Err(format!(
                "{where_}: the value-producing call {instruction:?} is not immediately followed by \
                 its result store `move x0 -> y{home}` (got {store:?})"
            )
            .into());
        }
    }
    Ok(())
}

/// Checks that argument register `register` reaching the call at `call` is
/// marshaled from the expected source. An `InPlace` argument only needs a
/// reaching write; every other kind must be a `move` from the exact source.
fn check_arg(
    where_: &str,
    code: &[Instruction],
    call: usize,
    register: u32,
    expected: &ArgSrc,
    ctx: &Decoded<'_>,
) -> CheckResult {
    let reaching = reaching_write(code, call, register);
    if matches!(expected, ArgSrc::InPlace) {
        return match reaching {
            Some(_) => Ok(()),
            None => Err(format!(
                "{where_}: the in-place argument x{register} has no reaching definition before the \
                 call"
            )
            .into()),
        };
    }
    let Some(Instruction::Move { source, .. }) = reaching else {
        return Err(format!(
            "{where_}: argument x{register} is not marshaled by a `move` before the call (reaching \
             def {reaching:?})"
        )
        .into());
    };
    if source_matches(expected, source, ctx) {
        Ok(())
    } else {
        Err(format!(
            "{where_}: argument x{register} is marshaled from {source:?}, not the source the \
             selected step names"
        )
        .into())
    }
}

/// The instruction whose write to `register` reaches the call at `call` — the
/// nearest preceding writer, stopping at a `Label` (a control-flow join breaks
/// the straight-line marshal block).
fn reaching_write(code: &[Instruction], call: usize, register: u32) -> Option<&Instruction> {
    for index in (0..call).rev() {
        let instruction = code.get(index)?;
        if matches!(instruction, Instruction::Label { .. }) {
            return None;
        }
        if reads_writes(instruction).1.contains(&register) {
            return Some(instruction);
        }
    }
    None
}

/// Whether a decoded marshal `move` source matches the expected argument source.
/// Atoms are compared by NAME across the emit-side and decode-side tables (their
/// interned indices differ between the two).
fn source_matches(expected: &ArgSrc, actual: &Operand, ctx: &Decoded<'_>) -> bool {
    match expected {
        ArgSrc::Home(home) => matches!(actual, Operand::Y(y) if y == home),
        ArgSrc::Lit(index) => matches!(actual, Operand::Literal(l) if l == index),
        ArgSrc::Int(value) => matches!(actual, Operand::Integer(v) if v == value),
        ArgSrc::Nil => matches!(actual, Operand::Atom(None)),
        ArgSrc::Atom(atom) => match actual {
            Operand::Atom(Some(decoded)) => {
                name_of(ctx.table, *decoded) == name_of(ctx.emit_atoms, *atom)
            }
            _ => false,
        },
        ArgSrc::InPlace => true,
    }
}

/// Rejects a decoded call whose self-declared arity differs from the selected
/// step's argument count.
fn require_arity(
    where_: &str,
    actual: &Operand,
    expected: u32,
    instruction: &Instruction,
) -> CheckResult {
    let declared = as_unsigned(actual).and_then(|value| u32::try_from(value).ok());
    if declared == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "{where_}: call declares arity {declared:?} but the selected step passes {expected}: \
             {instruction:?}"
        )
        .into())
    }
}

/// Rejects a decoded external call whose import operand names a different pool
/// index than the selected step.
fn require_target(
    where_: &str,
    actual: &Operand,
    expected: usize,
    kind: &str,
    instruction: &Instruction,
) -> CheckResult {
    if operand_index(actual) == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "{where_}: call names {kind} {:?}, not the selected step's {expected}: {instruction:?}",
            operand_index(actual)
        )
        .into())
    }
}

/// Rejects a decoded local call whose label operand names a different body label
/// than the selected step.
fn require_label(
    where_: &str,
    actual: &Operand,
    expected: u32,
    instruction: &Instruction,
) -> CheckResult {
    if matches!(actual, Operand::Label(label) if *label == expected) {
        Ok(())
    } else {
        Err(format!(
            "{where_}: call targets {actual:?}, not the selected step's label {expected}: \
             {instruction:?}"
        )
        .into())
    }
}

/// Maps a selected-IR source to its expected marshal source through the frame
/// homes.
fn arg_src(src: &Src, homes: &HashMap<Var, u32>) -> Result<ArgSrc, Box<dyn std::error::Error>> {
    Ok(match src {
        Src::Var(var) => ArgSrc::Home(
            *homes
                .get(var)
                .ok_or_else(|| format!("var {} has no frame home", var.0))?,
        ),
        Src::Lit(index) => ArgSrc::Lit(*index),
        Src::Int(value) => ArgSrc::Int(*value),
        Src::Atom(atom) => ArgSrc::Atom(*atom),
        Src::Nil => ArgSrc::Nil,
    })
}

/// Maps a list of sources to expected argument sources.
fn arg_srcs(
    srcs: &[Src],
    homes: &HashMap<Var, u32>,
) -> Result<Vec<ArgSrc>, Box<dyn std::error::Error>> {
    srcs.iter().map(|src| arg_src(src, homes)).collect()
}

/// The `Y` home a value-producing call stores its result into, when it has one.
fn produces(
    dst: Option<Var>,
    homes: &HashMap<Var, u32>,
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    match dst {
        Some(var) => Ok(Some(*homes.get(&var).ok_or_else(|| {
            format!("call result var {} has no frame home", var.0)
        })?)),
        None => Ok(None),
    }
}

/// Walks a block's steps then tail, appending call expectations in emission order.
fn collect_block(
    steps: &[Step],
    tail: &TailKind,
    homes: &HashMap<Var, u32>,
    out: &mut Vec<CallExpect>,
) -> CheckResult {
    for step in steps {
        collect_step(step, homes, out)?;
    }
    collect_tail(tail, homes, out)
}

/// Appends the call expectation(s) a single step emits (data/test steps emit
/// none; `json.object` emits one encode call per pair then the object call).
fn collect_step(step: &Step, homes: &HashMap<Var, u32>, out: &mut Vec<CallExpect>) -> CheckResult {
    match step {
        Step::CallImport {
            dst, import, args, ..
        } => out.push(CallExpect {
            kind: ExpectKind::Import(*import),
            args: arg_srcs(args, homes)?,
            produces: produces(*dst, homes)?,
        }),
        Step::CallLocal {
            dst, label, args, ..
        } => out.push(CallExpect {
            kind: ExpectKind::Local(*label),
            args: arg_srcs(args, homes)?,
            produces: produces(*dst, homes)?,
        }),
        Step::CallFun { dst, fun, args } => out.push(CallExpect {
            kind: ExpectKind::Fun(arg_src(fun, homes)?),
            args: arg_srcs(args, homes)?,
            produces: produces(*dst, homes)?,
        }),
        Step::MakeClosure { dst, captures, .. } => out.push(CallExpect {
            kind: ExpectKind::Closure,
            args: arg_srcs(captures, homes)?,
            produces: produces(Some(*dst), homes)?,
        }),
        Step::JsonObj {
            dst,
            pairs,
            object_import,
        } => {
            // Pairs are encoded in REVERSE order (emit.rs), each preceded by a
            // reload of its value into x0; then the assembled list (in place)
            // feeds the object call, whose result is stored into `dst`.
            for pair in pairs.iter().rev() {
                let kind = match pair.via {
                    Via::Import(import) => ExpectKind::Import(import),
                    Via::Local(label) => ExpectKind::Local(label),
                };
                out.push(CallExpect {
                    kind,
                    args: vec![arg_src(&pair.value, homes)?],
                    produces: None,
                });
            }
            out.push(CallExpect {
                kind: ExpectKind::Import(*object_import),
                args: vec![ArgSrc::InPlace],
                produces: produces(Some(*dst), homes)?,
            });
        }
        _ => {}
    }
    Ok(())
}

/// Appends the call expectation(s) a tail emits, recursing into branch arms.
fn collect_tail(
    tail: &TailKind,
    homes: &HashMap<Var, u32>,
    out: &mut Vec<CallExpect>,
) -> CheckResult {
    match tail {
        TailKind::Return(_) => {}
        TailKind::TailImport { import, args, .. } => out.push(CallExpect {
            kind: ExpectKind::TailImport(*import),
            args: arg_srcs(args, homes)?,
            produces: None,
        }),
        TailKind::TailLocal { label, args, .. } => out.push(CallExpect {
            kind: ExpectKind::TailLocal(*label),
            args: arg_srcs(args, homes)?,
            produces: None,
        }),
        TailKind::If {
            then_block,
            else_block,
            ..
        } => {
            collect_block(&then_block.steps, &then_block.tail, homes, out)?;
            collect_block(&else_block.steps, &else_block.tail, homes, out)?;
        }
        TailKind::SelectEnum { arms, .. } => {
            for (_, block) in arms {
                collect_block(&block.steps, &block.tail, homes, out)?;
            }
        }
    }
    Ok(())
}
