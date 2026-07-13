//! S14 backward liveness: annotate every `CallRt`/`CallLocal`/`CallClosure`/
//! `TryBind` with the set of vars live across it — the y-spill contract handed
//! to BC-3 as data (printed in goldens). A single backward walk per `FlowFn`
//! body; branch tails merge their arms' live-in sets.
//!
//! `live_after(op) = live_out(op) \ defs(op)`: the vars that must survive the
//! call in y-registers (the op's own result lands in the return register, so it
//! is excluded). Sets are rendered in ascending `Var` order for determinism.

use std::collections::BTreeSet;

use super::super::func::MirFn;
use super::super::ids::Var;
use super::super::ops::{Block, JsonVal, LiveAfter, Stmt, Tail, Test, Value};
use super::super::unit::MirModule;

/// Fill `live_after` on every call/bind op in every flow function.
pub(super) fn annotate(module: &mut MirModule) {
    for function in &mut module.functions {
        if let MirFn::Flow(flow) = function {
            let _ = block_live_in(&mut flow.body);
        }
    }
}

/// Annotate `block` in place and return the set of vars live on entry to it.
/// A block's tail is terminal, so nothing outlives the block itself.
fn block_live_in(block: &mut Block) -> BTreeSet<Var> {
    let mut live = tail_live_in(&mut block.tail);
    for stmt in block.stmts.iter_mut().rev() {
        let live_out = live.clone();
        let defs = defs(stmt);
        set_live_after(stmt, &live_out, &defs);
        for def in &defs {
            live.remove(def);
        }
        annotate_nested(stmt);
        for used in uses(stmt) {
            live.insert(used);
        }
    }
    live
}

fn tail_live_in(tail: &mut Tail) -> BTreeSet<Var> {
    match tail {
        Tail::Return(value) => single(value),
        Tail::TailLocal { args, .. } | Tail::TailRt { args, .. } => value_set(args),
        Tail::If {
            test,
            then_block,
            else_block,
            ..
        } => {
            let mut live = test_uses(test);
            live.extend(block_live_in(then_block));
            live.extend(block_live_in(else_block));
            live
        }
        Tail::SelectEnum { subject, arms, .. } => {
            let mut live = single(subject);
            for (_, arm) in arms.iter_mut() {
                live.extend(block_live_in(arm));
            }
            live
        }
    }
}

/// Recurse into an op's nested blocks (`Attempt`), which run within the
/// enclosing frame. These shapes are outside the covered set today, but the
/// pass stays total so it never panics on them.
fn annotate_nested(stmt: &mut Stmt) {
    if let Stmt::Attempt { on_ok, on_err, .. } = stmt {
        let _ = block_live_in(on_ok);
        let _ = block_live_in(on_err);
    }
}

fn set_live_after(stmt: &mut Stmt, live_out: &BTreeSet<Var>, defs: &BTreeSet<Var>) {
    let across = || {
        let mut vars: Vec<Var> = live_out.difference(defs).copied().collect();
        vars.sort_unstable();
        LiveAfter(vars)
    };
    match stmt {
        Stmt::CallRt { live_after, .. }
        | Stmt::CallLocal { live_after, .. }
        | Stmt::CallClosure { live_after, .. }
        | Stmt::TryBind { live_after, .. } => *live_after = across(),
        _ => {}
    }
}

fn defs(stmt: &Stmt) -> BTreeSet<Var> {
    let mut set = BTreeSet::new();
    if let Some(def) = stmt.defined() {
        set.insert(def);
    }
    if let Stmt::AssertList { binds, .. } = stmt {
        for bind in binds.iter().flatten() {
            set.insert(*bind);
        }
    }
    set
}

fn uses(stmt: &Stmt) -> BTreeSet<Var> {
    match stmt {
        Stmt::Bind { value, .. } | Stmt::FieldGet { base: value, .. } => single(value),
        Stmt::RecordNew { args, .. }
        | Stmt::TupleNew { items: args, .. }
        | Stmt::ListNew { items: args, .. }
        | Stmt::CallRt { args, .. }
        | Stmt::CallLocal { args, .. } => value_set(args),
        Stmt::CallClosure { fun, args, .. } => {
            let mut set = single(fun);
            set.extend(value_set(args));
            set
        }
        Stmt::MakeClosure { captures, .. }
        | Stmt::WaitTimeoutCase { captures, .. }
        | Stmt::Attempt { captures, .. } => value_set(captures),
        Stmt::TryBind { result, .. } => BTreeSet::from([*result]),
        Stmt::Cmp { lhs, rhs, .. }
        | Stmt::BoolOp { lhs, rhs, .. }
        | Stmt::Concat { lhs, rhs, .. } => {
            let mut set = single(lhs);
            set.extend(single(rhs));
            set
        }
        Stmt::Not { src, .. } => single(src),
        Stmt::Increment { src, .. } => BTreeSet::from([*src]),
        Stmt::AssertList { list, .. } => BTreeSet::from([*list]),
        Stmt::AssertSome { option, .. } => BTreeSet::from([*option]),
        Stmt::IndexGuard { base, .. } => BTreeSet::from([*base]),
        Stmt::JsonObj { pairs, .. } => {
            let mut set = BTreeSet::new();
            for (_, JsonVal::Encoded { value, .. }) in pairs {
                set.extend(single(value));
            }
            set
        }
    }
}

fn test_uses(test: &Test) -> BTreeSet<Var> {
    match test {
        Test::IsTrue(value) | Test::IsTagged { value, .. } => single(value),
        Test::Cmp { lhs, rhs, .. } => {
            let mut set = single(lhs);
            set.extend(single(rhs));
            set
        }
        Test::Not(inner) => test_uses(inner),
    }
}

fn single(value: &Value) -> BTreeSet<Var> {
    match value {
        Value::Var(var) => BTreeSet::from([*var]),
        _ => BTreeSet::new(),
    }
}

fn value_set(values: &[Value]) -> BTreeSet<Var> {
    let mut set = BTreeSet::new();
    for value in values {
        set.extend(single(value));
    }
    set
}
