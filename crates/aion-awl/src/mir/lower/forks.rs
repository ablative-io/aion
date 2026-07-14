//! Fork lowering — the MIR twin of the reference `emitter/forks.rs`, to the
//! emitter's parity contract, NOT the checker's broader acceptance:
//!
//! - collection `fork item in expr` (parallel, one unbound ACTION call):
//!   a lifted branch-body fn returning the unrun configured activity value,
//!   dispatched through `workflow.map` (input-order results, engine-owned
//!   fail-fast) `|> map_activity_error`, `TryBind`;
//! - collection child forks: parallel fan-out uses one `try_fold` to spawn
//!   every string-name child and a second `try_fold` to await the reversed
//!   handle list while prepending results (spawn-all, ordered-await, input-order
//!   results); sequential fan-out uses `spawn_and_wait` in one fold followed by
//!   `list.reverse`;
//! - collection `… sequential`: `list.try_fold(items, [], fn(acc, item))`
//!   running each activity in input order, prepending, then `list.reverse` —
//!   joined results are input-ordered;
//! - named fork, homogeneous action branches: source-order activity values in
//!   ONE typed `workflow.all`, destructured by `AssertList` in source order;
//! - named fork, heterogeneous action branches: each branch rides its raw
//!   wrapper twin (wire bytes identical, `Activity(String, String)`) in one
//!   `workflow.all`, and the join decodes each bound position with that
//!   action's return codec and string action name (`awlc.decoded/3`).
//!
//! Everything the reference refuses, we refuse (clean `Unsupported`):
//! multi-statement/bound collection bodies, parallel indexing preludes,
//! named-child branches, non-action named branches.
//!
//! ONE deliberate parity exception (BC-2b-5, recorded in AWL-BC-IR.md): the
//! reference emitter passes call-site config on fork branches through
//! `activity_value` (`emitter/forks.rs:218-229,300-336,351-365`), while
//! direct lowering refuses it with the global BC-2 `call-site config` scope
//! class — full support needs per-key site/declaration merge across the
//! typed and raw call paths and stays deferred with the global marker
//! (`tests/compile.rs::call_site_node_override_yields_the_override`;
//! fork-form pins in `mir/fork_tests.rs`).

use std::collections::BTreeSet;

use crate::ast::{CallStmt, Expr, ForkHeader, ForkStmt, Statement, Step};
use crate::emitter::{Emitter, GType, expr_refs, type_ref_to_g};
use crate::spanned::Spanned;

use super::super::ops::Stmt;
use super::build::FnPlan;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_expr};
use super::slots::Slots;

/// The fork-function inventory a document's regions will consume, in the
/// exact traversal order lowering encounters them: statements in written
/// order with the `lower_step` early-stop, descending into loop bodies
/// pre-order (a fork inside a loop body consumes its slot while the loop fn
/// lowers). Only the shapes that lower consume a slot: a collection fork
/// whose sole branch is one unbound ACTION call takes one lifted fn (map body
/// or fold body); a child call takes one sequential folder or the parallel
/// spawn+await pair; named forks build inline and take none; every refused
/// shape errors before consuming.
pub(super) fn count_fork_fns(statements: &[Statement], emitter: &Emitter<'_>) -> u32 {
    let mut count = 0;
    for statement in statements {
        match statement {
            Statement::Fork(fork) => count += fork_fn_count(fork, emitter),
            Statement::Loop(looped) => count += count_fork_fns(&looped.body, emitter),
            Statement::Route(_) => break,
            Statement::Pipe(pipe) if matches!(pipe.end, crate::ast::PipeEnd::Route(_)) => break,
            _ => {}
        }
    }
    count
}

fn fork_fn_count(fork: &ForkStmt, emitter: &Emitter<'_>) -> u32 {
    match &fork.header {
        ForkHeader::Collection { sequential, .. } => match single_unbound_call(&fork.body) {
            Some(call) if emitter.actions.contains_key(call.call.name.as_str()) => 1,
            Some(call) if emitter.children.contains_key(call.call.name.as_str()) => {
                if *sequential {
                    1
                } else {
                    2
                }
            }
            _ => 0,
        },
        ForkHeader::Named => 0,
    }
}

/// Whether reachable lowering traversal contains a child collection fork or a
/// child-naming pipe stage and therefore needs the one module-local T-WIT
/// function. A route-ended pipe checks its own stages BEFORE honoring the
/// early stop.
pub(super) fn needs_child_witness(statements: &[Statement], emitter: &Emitter<'_>) -> bool {
    for statement in statements {
        match statement {
            Statement::Fork(fork) if matches!(fork.header, ForkHeader::Collection { .. }) => {
                if single_unbound_call(&fork.body)
                    .is_some_and(|call| emitter.children.contains_key(call.call.name.as_str()))
                {
                    return true;
                }
            }
            Statement::Loop(looped) => {
                if needs_child_witness(&looped.body, emitter) {
                    return true;
                }
            }
            Statement::Route(_) => break,
            Statement::Pipe(pipe) => {
                if pipe.stages.iter().any(|stage| {
                    matches!(stage,
                        crate::ast::PipeStage::Action { name, .. }
                            if emitter.children.contains_key(name.as_str()))
                }) {
                    return true;
                }
                if matches!(pipe.end, crate::ast::PipeEnd::Route(_)) {
                    break;
                }
            }
            _ => {}
        }
    }
    false
}

/// Every action a heterogeneous named fork dispatches — these need the raw
/// wrapper twin planned (`build::raw_activity_shell`). Sorted (`BTreeSet`) for
/// deterministic slot order; the traversal mirrors `count_fork_fns`.
pub(super) fn raw_action_inventory(emitter: &Emitter<'_>) -> Vec<String> {
    let mut out = BTreeSet::new();
    for step in &emitter.document.steps {
        collect_raw_actions(&step.body, emitter, &mut out);
    }
    out.into_iter().collect()
}

fn collect_raw_actions(
    statements: &[Statement],
    emitter: &Emitter<'_>,
    out: &mut BTreeSet<String>,
) {
    for statement in statements {
        match statement {
            Statement::Fork(fork) if matches!(fork.header, ForkHeader::Named) => {
                let mut names = Vec::new();
                let mut all_actions = true;
                for branch in &fork.body {
                    match branch {
                        Statement::Call(call)
                            if emitter.actions.contains_key(call.call.name.as_str()) =>
                        {
                            names.push(call.call.name.clone());
                        }
                        _ => {
                            all_actions = false;
                            break;
                        }
                    }
                }
                let heterogeneous = names.len() > 1 && names.iter().any(|name| *name != names[0]);
                if all_actions && heterogeneous {
                    out.extend(names);
                }
            }
            Statement::Loop(looped) => collect_raw_actions(&looped.body, emitter, out),
            Statement::Route(_) => break,
            Statement::Pipe(pipe) if matches!(pipe.end, crate::ast::PipeEnd::Route(_)) => break,
            _ => {}
        }
    }
}

fn single_unbound_call(body: &[Statement]) -> Option<&CallStmt> {
    match body {
        [Statement::Call(call)] if call.bind.is_none() => Some(call),
        _ => None,
    }
}

pub(super) fn lower_fork_stmt(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    fork: &ForkStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    match &fork.header {
        ForkHeader::Collection {
            var,
            collection,
            sequential,
            ..
        } => lower_collection_fork(
            ctx,
            plan,
            &CollectionFork {
                step,
                fork,
                var,
                collection,
                sequential: *sequential,
            },
            scope,
            stmts,
            slots,
        ),
        ForkHeader::Named => super::fork_named::lower_named_fork(ctx, plan, fork, scope, stmts),
    }
}

struct CollectionFork<'a> {
    step: &'a Step,
    fork: &'a ForkStmt,
    var: &'a str,
    collection: &'a Expr,
    sequential: bool,
}

fn lower_collection_fork(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    fork: &CollectionFork<'_>,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let branch = collection_branch(ctx, fork.fork, fork.sequential)?;

    // R4: the collection expression evaluates BEFORE fan-out.
    let (items_value, items_ty) = lower_expr(ctx, fork.collection, scope, stmts)?;
    let elem_ty = match ctx.emitter.env.resolve(&items_ty) {
        GType::List(inner) => *inner,
        other => {
            return Err(LowerError::new(
                fork.collection.span(),
                format!(
                    "`fork … in` needs a list, found {}",
                    ctx.emitter.env.gleam_type(&other)
                ),
            ));
        }
    };
    let free = branch_free_names(branch.call, fork.var, scope);
    let joined = match branch.kind {
        CollectionKind::Action => super::fork_action::lower_action_collection(
            ctx,
            super::fork_action::ActionFork {
                plan,
                step: fork.step,
                fork: fork.fork,
                call: branch.call,
                var: fork.var,
                returns: &branch.returns,
                items: items_value,
                elem_ty: &elem_ty,
                free: &free,
                scope,
                sequential: fork.sequential,
            },
            stmts,
            slots,
        )?,
        CollectionKind::Child => super::fork_child::lower_child_collection(
            ctx,
            plan,
            &super::fork_child::ChildFork {
                step: fork.step,
                fork: fork.fork,
                call: branch.call,
                var: fork.var,
                items: items_value,
                elem_ty: &elem_ty,
                returns: &branch.returns,
                free: &free,
                sequential: fork.sequential,
            },
            scope,
            stmts,
            slots,
        )?,
    };
    if let Some(bind) = &fork.fork.join.bind {
        scope.insert(
            bind.name.clone(),
            Binding {
                var: joined,
                ty: GType::List(Box::new(branch.returns)),
            },
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum CollectionKind {
    Action,
    Child,
}

struct CollectionBranch<'a> {
    call: &'a crate::ast::Call,
    returns: GType,
    kind: CollectionKind,
}

/// The reference stopgap gate for a collection fork body: exactly one
/// unbound action or child call — everything else refuses with the emitter's
/// diagnostic class (multi-statement/bound bodies, call-site config, parallel
/// indexing preludes).
fn collection_branch<'f>(
    ctx: &Ctx<'_>,
    fork: &'f ForkStmt,
    sequential: bool,
) -> Result<CollectionBranch<'f>, LowerError> {
    let Some(branch) = single_unbound_call(&fork.body) else {
        // The reference stopgap: one unbound call per item, nothing else.
        return Err(LowerError::unsupported(
            "a collection fork body beyond one unbound call",
            fork.span,
        ));
    };
    if branch.config.is_some() {
        return Err(LowerError::unsupported("call-site config", branch.span));
    }
    let call = &branch.call;
    let (kind, returns) = if let Some(&(_, decl)) = ctx.emitter.actions.get(call.name.as_str()) {
        (CollectionKind::Action, type_ref_to_g(&decl.returns))
    } else if let Some(child) = ctx.emitter.children.get(call.name.as_str()) {
        (CollectionKind::Child, type_ref_to_g(&child.returns))
    } else {
        return Err(LowerError::new(
            call.name_span,
            format!(
                "`{}` names neither a declared action nor a child workflow",
                call.name
            ),
        ));
    };
    if !sequential && args_contain_index(call) {
        // The reference refuses indexing preludes inside a PARALLEL branch.
        return Err(LowerError::unsupported(
            "indexing inside a parallel fork branch",
            call.span,
        ));
    }
    Ok(CollectionBranch {
        call,
        returns,
        kind,
    })
}

/// Branch-call refs beyond the loop var, restricted to names the call site
/// can supply — sorted (`BTreeSet`) so capture order is deterministic (R4).
pub(super) fn branch_free_names(call: &crate::ast::Call, var: &str, scope: &Scope) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for arg in &call.args {
        expr_refs(&arg.value, &mut refs);
    }
    refs.remove(var);
    refs.into_iter()
        .filter(|name| scope.contains_key(name))
        .collect()
}

fn args_contain_index(call: &crate::ast::Call) -> bool {
    call.args.iter().any(|arg| expr_contains_index(&arg.value))
}

fn expr_contains_index(expr: &Expr) -> bool {
    match expr {
        Expr::Index { .. } => true,
        Expr::Field { base, .. } | Expr::Not { expr: base, .. } => expr_contains_index(base),
        Expr::Binary { left, right, .. } => expr_contains_index(left) || expr_contains_index(right),
        Expr::Predicate { subject, .. } => expr_contains_index(subject),
        Expr::Record { args, .. } => args.iter().any(|arg| expr_contains_index(&arg.value)),
        Expr::List { items, .. } => items.iter().any(expr_contains_index),
        _ => false,
    }
}
