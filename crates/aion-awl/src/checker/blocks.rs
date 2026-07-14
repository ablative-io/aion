//! Block statements of the flow walk: `fork … join` (collection and
//! named-branch forms) and `loop … until … max`.
//!
//! Named-branch forks run their branches in parallel, so each branch walks
//! in its own clone of the pre-fork scope — a branch can never read a
//! sibling's binding — and every branch's bindings merge into the outer
//! scope only after `join`.

use std::rc::Rc;

use crate::Span;
use crate::ast::{Expr, ForkHeader, ForkStmt, LoopStmt, PipeEnd, Statement, Step};
use crate::spanned::Spanned;

use super::exprs::{View, type_of};
use super::outcomes::Env;
use super::types::{Ty, resolve};
use super::walk::{LoopFrame, Scope, Walker, insert_binding, walk_statements};

pub(super) fn walk_fork(
    w: &mut Walker<'_, '_>,
    scope: &mut Scope,
    fork: &ForkStmt,
    owner: &Step,
    env: &Env<'_>,
) -> Option<Ty> {
    match &fork.header {
        ForkHeader::Collection {
            var,
            var_span,
            collection,
            ..
        } => {
            let view = View {
                vars: scope,
                narrow: None,
                accessor: None,
            };
            let collection_ty = type_of(w, view, collection);
            let element = match resolve(&collection_ty, &w.ctx.types) {
                Ty::List(inner) => (*inner).clone(),
                Ty::Unknown => Ty::Unknown,
                other => {
                    w.err(
                        collection.span(),
                        format!("`fork … in` needs a list to fan out over, found {other}"),
                    );
                    Ty::Unknown
                }
            };
            let mut branch = scope.clone();
            w.fork_depth += 1;
            insert_binding(w, &mut branch, var, element, *var_span);
            let produced = walk_statements(w, &mut branch, &fork.body, owner, env);
            w.fork_depth -= 1;
            if let Some(bind) = &fork.join.bind {
                let joined = Ty::List(Rc::new(produced.unwrap_or(Ty::Unknown)));
                insert_binding(w, scope, &bind.name, joined, bind.span);
            }
        }
        ForkHeader::Named => {
            if let Some(bind) = &fork.join.bind {
                w.err(
                    bind.span,
                    "a named-branch fork joins without a binding — each branch \
                     carries its own (`join` takes no `->`)",
                );
            }
            let base = scope.clone();
            let mut merges: Vec<(String, Ty, Span)> = Vec::new();
            for branch in &fork.body {
                let mut branch_scope = base.clone();
                walk_statements(
                    w,
                    &mut branch_scope,
                    std::slice::from_ref(branch),
                    owner,
                    env,
                );
                let mut binds = Vec::new();
                statement_binds(branch, &mut binds);
                for (name, span) in binds {
                    // Names already in the pre-fork scope were reported (or
                    // counted as the loop rebind) during the branch walk.
                    if base.contains_key(&name) {
                        continue;
                    }
                    if let Some(ty) = branch_scope.get(&name) {
                        merges.push((name, ty.ty.clone(), span));
                    }
                }
            }
            for (name, ty, span) in merges {
                insert_binding(w, scope, &name, ty, span);
            }
        }
    }
    None
}

/// Every name a branch statement binds, with the binding's span — the
/// spanned sibling of `avail::defined_in_statements`.
fn statement_binds(statement: &Statement, out: &mut Vec<(String, Span)>) {
    match statement {
        Statement::Call(call) => {
            if let Some(bind) = &call.bind {
                out.push((bind.name.clone(), bind.span));
            }
        }
        Statement::Pipe(pipe) => {
            if let PipeEnd::Bind(bind) = &pipe.end {
                out.push((bind.name.clone(), bind.span));
            }
        }
        Statement::Wait(wait) => {
            out.push((wait.bind.name.clone(), wait.bind.span));
        }
        Statement::Fork(fork) => {
            if matches!(fork.header, ForkHeader::Named) {
                for inner in &fork.body {
                    statement_binds(inner, out);
                }
            }
            if let Some(bind) = &fork.join.bind {
                out.push((bind.name.clone(), bind.span));
            }
        }
        Statement::Loop(looped) => {
            out.push((looped.var.clone(), looped.var_span));
            if let Some(counter) = &looped.counter {
                out.push((counter.name.clone(), counter.span));
            }
        }
        Statement::SubStep(sub) => {
            for inner in &sub.body {
                statement_binds(inner, out);
            }
        }
        Statement::Spawn(_) | Statement::Sleep(_) | Statement::Route(_) => {}
    }
}

pub(super) fn walk_loop(
    w: &mut Walker<'_, '_>,
    scope: &mut Scope,
    looped: &LoopStmt,
    owner: &Step,
    env: &Env<'_>,
) -> Option<Ty> {
    let view = View::plain(scope);
    let counter_collides = looped
        .counter
        .as_ref()
        .is_some_and(|counter| counter.name == looped.var);
    if let Some(counter) = &looped.counter
        && counter_collides
    {
        w.err(
            counter.span,
            "`counting` name must differ from the loop binding",
        );
    }
    let seed_ty = type_of(w, view, &looped.seed);
    // `max` is a ceiling fixed before the first pass: an expression over
    // inputs and prior bindings, typed in the pre-loop scope (the emitter
    // renders it at the loop call site, where loop-locals do not exist).
    let pre_loop = scope.clone();
    insert_binding(w, scope, &looped.var, seed_ty.clone(), looped.var_span);
    let fork_depth = w.fork_depth;
    w.loops.push(LoopFrame {
        var: looped.var.clone(),
        seed_ty,
        rebound: false,
        fork_depth,
    });
    let mut body_scope = scope.clone();
    walk_statements(w, &mut body_scope, &looped.body, owner, env);
    let body_view = View::plain(&body_scope);
    match &looped.until {
        Some(tail) => {
            let ty = type_of(w, body_view, &tail.expr);
            if !matches!(resolve(&ty, &w.ctx.types), Ty::Bool | Ty::Unknown) {
                w.err(
                    tail.expr.span(),
                    format!("`until` needs a Bool condition, found {ty}"),
                );
            }
        }
        None => {
            w.err(
                looped.span,
                "a loop must declare an `until` condition — the body must be able to stop",
            );
        }
    }
    match &looped.max {
        Some(tail) => {
            if let Some((name, span)) = loop_local_ref(&tail.expr, looped) {
                w.err(
                    span,
                    format!(
                        "`max` is the loop's ceiling, fixed before the first pass — \
                         `{name}` is loop-local; the bound must be an expression over \
                         inputs and prior bindings"
                    ),
                );
            } else {
                let pre_view = View::plain(&pre_loop);
                let ty = type_of(w, pre_view, &tail.expr);
                if !matches!(resolve(&ty, &w.ctx.types), Ty::Int | Ty::Unknown) {
                    w.err(
                        tail.expr.span(),
                        format!("`max` needs an Int ceiling, found {ty}"),
                    );
                }
            }
        }
        None => {
            w.err(
                looped.span,
                "this loop is unbounded — `max` is mandatory; unbounded \
                 `loop … until` is illegal",
            );
        }
    }
    if let Some(frame) = w.loops.pop()
        && !frame.rebound
    {
        w.err(
            looped.span,
            format!(
                "the loop body never rebinds `{}` — the threaded value must be \
                 rebound (`-> {}`) so the next pass sees it",
                looped.var, looped.var
            ),
        );
    }
    if let Some(counter) = &looped.counter
        && !counter_collides
    {
        insert_binding(w, scope, &counter.name, Ty::Int, counter.span);
    }
    None
}

/// The first reference to the loop's threaded value or counter inside an
/// expression — `max` may not read either (the ceiling is loop-invariant).
fn loop_local_ref(expr: &Expr, looped: &LoopStmt) -> Option<(String, Span)> {
    let is_local = |name: &str| {
        name == looped.var
            || looped
                .counter
                .as_ref()
                .is_some_and(|counter| counter.name == name)
    };
    match expr {
        Expr::Ref { span, name } => is_local(name).then(|| (name.clone(), *span)),
        Expr::String { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::Variant { .. }
        | Expr::Accessor { .. }
        | Expr::Workflow { .. } => None,
        Expr::List { items, .. } => items.iter().find_map(|item| loop_local_ref(item, looped)),
        Expr::Record { args, .. } => args
            .iter()
            .find_map(|arg| loop_local_ref(&arg.value, looped)),
        Expr::Field { base, .. } | Expr::Index { base, .. } => loop_local_ref(base, looped),
        Expr::Not { expr: inner, .. } => loop_local_ref(inner, looped),
        Expr::Binary { left, right, .. } => {
            loop_local_ref(left, looped).or_else(|| loop_local_ref(right, looped))
        }
        Expr::Predicate { subject, .. } => loop_local_ref(subject, looped),
        Expr::CollectionPredicate {
            collection,
            predicate,
            ..
        } => loop_local_ref(collection, looped).or_else(|| loop_local_ref(predicate, looped)),
    }
}
