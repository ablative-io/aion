//! Call-shaped statements of the flow walk: action/child/subflow calls,
//! `spawn`, `wait`, and the `max … visits` step attribute's bound checking.

use crate::ast::{CallStmt, Expr, RetrySpec, SpawnStmt, Step, WaitStmt};
use crate::semantic::DeclarationKind;
use crate::spanned::Spanned;

use super::args::check_args;
use super::context::Callable;
use super::exprs::{View, type_of};
use super::types::{Ty, resolve};
use super::walk::{Scope, Walker, insert_binding};

pub(super) fn walk_call(w: &mut Walker<'_, '_>, scope: &mut Scope, call: &CallStmt) -> Ty {
    let view = View {
        vars: scope,
        narrow: None,
        accessor: None,
    };
    let name = &call.call.name;
    let resolved: Option<(Callable, &'static str, DeclarationKind)> =
        if let Some(action) = w.ctx.actions.get(name) {
            Some((action.clone(), "action", DeclarationKind::Action))
        } else if let Some(child) = w.ctx.children.get(name) {
            Some((child.clone(), "child", DeclarationKind::Child))
        } else {
            w.ctx
                .subflows
                .get(name)
                .map(|subflow| (subflow.clone(), "subflow", DeclarationKind::Subflow))
        };
    let returns = match resolved {
        None => {
            w.err(
                call.call.name_span,
                format!("no action, child, or subflow named `{name}` is declared"),
            );
            Ty::Unknown
        }
        Some((callable, kind, declaration_kind)) => {
            if w.emit {
                w.ctx
                    .semantic
                    .reference(call.call.name_span, declaration_kind, name);
                w.ctx
                    .semantic
                    .ty(call.call.name_span, &callable.returns.to_string());
            }
            let params: Vec<(String, Ty)> = callable
                .params
                .iter()
                .map(|param| (param.name.clone(), param.ty.clone()))
                .collect();
            check_args(
                w,
                view,
                &call.call.args,
                &params,
                &format!("{kind} `{name}`"),
                "argument",
                call.call.name_span,
            );
            callable.returns
        }
    };
    if let Some(config) = &call.config {
        if w.ctx.children.contains_key(name) {
            w.err(
                config.span,
                format!(
                    "a child call carries no call-site config — `node`/`timeout` pins \
                     apply to worker actions, and the engine routes children, not a \
                     queue (`{name}` is a child)"
                ),
            );
        } else if w.ctx.subflows.contains_key(name) {
            w.err(
                config.span,
                format!(
                    "a subflow call carries no call-site config — a subflow compiles \
                     inline; its own actions carry the pins (`{name}` is a subflow)"
                ),
            );
        } else if let Some(retry) = &config.retry {
            let span = match retry {
                RetrySpec::Every { span, .. } | RetrySpec::Backoff { span, .. } => *span,
            };
            w.err(
                span,
                "a call site may pin `node` and `timeout` only — `retry` stays on the \
                 action declaration",
            );
        }
    }
    if let Some(bind) = &call.bind {
        insert_binding(w, scope, &bind.name, returns.clone(), bind.span);
    }
    returns
}

pub(super) fn walk_spawn(
    w: &mut Walker<'_, '_>,
    scope: &mut Scope,
    spawn: &SpawnStmt,
) -> Option<Ty> {
    let view = View {
        vars: scope,
        narrow: None,
        accessor: None,
    };
    let name = &spawn.call.name;
    if let Some(child) = w.ctx.children.get(name).cloned() {
        if w.emit {
            w.ctx
                .semantic
                .reference(spawn.call.name_span, DeclarationKind::Child, name);
            w.ctx
                .semantic
                .ty(spawn.call.name_span, &child.returns.to_string());
        }
        let params: Vec<(String, Ty)> = child
            .params
            .iter()
            .map(|param| (param.name.clone(), param.ty.clone()))
            .collect();
        check_args(
            w,
            view,
            &spawn.call.args,
            &params,
            &format!("child `{name}`"),
            "argument",
            spawn.call.name_span,
        );
    } else if w.ctx.actions.contains_key(name) {
        w.err(
            spawn.call.name_span,
            format!(
                "`{name}` is a worker action — `spawn` starts a declared child \
                 workflow; call the action directly instead"
            ),
        );
    } else if w.ctx.subflows.contains_key(name) {
        w.err(
            spawn.call.name_span,
            format!(
                "`{name}` is a subflow — it runs inline as its own step; call it \
                 directly (`{name}(…) -> <binding>`), never `spawn` it"
            ),
        );
    } else {
        w.err(
            spawn.call.name_span,
            format!("`spawn` names an undeclared child workflow `{name}`"),
        );
    }
    if let Some(bind) = &spawn.bind {
        w.err(
            bind.span,
            "`spawn` is fire-and-forget — a spawned child cannot bind a result \
             (`->` after `spawn` is an error)",
        );
    }
    None
}

pub(super) fn walk_wait(w: &mut Walker<'_, '_>, scope: &mut Scope, wait: &WaitStmt) -> Ty {
    let ty = if let Some(payload) = w.ctx.signals.get(&wait.signal).cloned() {
        if w.emit {
            w.ctx
                .semantic
                .reference(wait.signal_span, DeclarationKind::Signal, &wait.signal);
            w.ctx.semantic.ty(wait.signal_span, &payload.to_string());
        }
        payload
    } else {
        w.err(
            wait.signal_span,
            format!(
                "no signal named `{}` is declared in the workflow header",
                wait.signal
            ),
        );
        Ty::Unknown
    };
    let bound = if wait.timeout.is_some() {
        ty.optional()
    } else {
        ty
    };
    insert_binding(w, scope, &wait.bind.name, bound.clone(), wait.bind.span);
    bound
}

/// Check a step's `max … visits` bound: an `Int` fixed before the flow
/// starts — derived from the flow's inputs and document consts only, never
/// from runtime bindings (a binding could itself be rewritten around the
/// cycle, and the bound must not move).
pub(super) fn check_max_visits(w: &mut Walker<'_, '_>, step: &Step) {
    let Some(max_visits) = &step.max_visits else {
        return;
    };
    if let Some((name, span)) = runtime_ref(w, &max_visits.bound) {
        w.err(
            span,
            format!(
                "the `max … visits` bound is fixed before the flow starts — `{name}` \
                 is a runtime binding; the bound must be an expression over inputs \
                 and consts"
            ),
        );
        return;
    }
    let inputs: Scope = w
        .flow
        .inputs
        .iter()
        .map(|(name, ty)| {
            (
                name.clone(),
                super::walk::ScopedTy {
                    ty: ty.clone(),
                    declaration: None,
                },
            )
        })
        .collect();
    let view = View::plain(&inputs);
    let ty = type_of(w, view, &max_visits.bound);
    if !matches!(resolve(&ty, &w.ctx.types), Ty::Int | Ty::Unknown) {
        w.err(
            max_visits.bound.span(),
            format!("`max … visits` needs an Int bound, found {ty}"),
        );
    }
}

/// The first reference in a visits bound that names neither a flow input
/// nor a const (`visits` itself included — the counter cannot bound
/// itself).
fn runtime_ref(w: &Walker<'_, '_>, expr: &Expr) -> Option<(String, crate::Span)> {
    match expr {
        Expr::Ref { span, name } => {
            let known = w.flow.inputs.contains_key(name) || w.ctx.consts.contains_key(name);
            (!known).then(|| (name.clone(), *span))
        }
        Expr::List { items, .. } => items.iter().find_map(|item| runtime_ref(w, item)),
        Expr::Record { args, .. } => args.iter().find_map(|arg| runtime_ref(w, &arg.value)),
        Expr::Field { base, .. } | Expr::Index { base, .. } => runtime_ref(w, base),
        Expr::Not { expr: inner, .. } => runtime_ref(w, inner),
        Expr::Binary { left, right, .. } => runtime_ref(w, left).or_else(|| runtime_ref(w, right)),
        Expr::Predicate { subject, .. } => runtime_ref(w, subject),
        Expr::CollectionPredicate {
            collection,
            predicate,
            ..
        } => runtime_ref(w, collection).or_else(|| runtime_ref(w, predicate)),
        Expr::String { .. }
        | Expr::RawString { .. }
        | Expr::Json { .. }
        | Expr::SchemaOf { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::Variant { .. }
        | Expr::Accessor { .. }
        | Expr::Workflow { .. } => None,
    }
}
