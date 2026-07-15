//! Outcome-clause checking: guard typing, guard-dependent optionality
//! narrowing, route-target resolution (steps, flow outcomes, substep
//! siblings, parent arms), payload contracts (constructed and value forms),
//! the `visits` builtin's guard scope, and `otherwise` placement.
//! Exhaustiveness lives in `exhaustive`.

use crate::Span;
use crate::ast::{Expr, Guard, PredicateKind, RoutePayload, RouteTarget, Statement, Step};
use crate::semantic::DeclarationKind;
use crate::spanned::Spanned;

use super::args::check_args;
use super::exhaustive::check_exhaustiveness;
use super::exprs::{View, type_of};
use super::types::{Ty, assignable, resolve};
use super::walk::{Scope, ScopedTy, Walker};

/// Where a route target resolves: the top level of the document, or inside
/// a substep group (siblings and the parent's outcome arms).
pub(super) enum Env<'e> {
    /// Top-level step surface: targets are steps and workflow outcomes.
    Top,
    /// Inside a substep of `parent`: targets are sibling substeps and the
    /// parent's outcome-arm names; anything beyond the parent is an escape.
    Substep {
        /// The step whose body contains this substep.
        parent: &'e Step,
        /// Names of the sibling substeps (including this one).
        siblings: Vec<String>,
    },
}

enum RouteKind {
    /// A top-level step.
    Step,
    /// A workflow outcome with its payload type.
    Outcome(String, Ty),
    /// A sibling substep.
    Sibling,
    /// A parent outcome arm.
    ParentArm,
    /// A name that exists only outside the parent step.
    Escapes(String),
    /// No such target anywhere.
    Unknown,
}

fn resolve_route(w: &Walker<'_, '_>, env: &Env<'_>, name: &str) -> RouteKind {
    let top_step = w.flow.steps.iter().any(|step| step.name == name);
    let outcome = w.flow.outcomes.get(name).cloned();
    match env {
        Env::Top => {
            if top_step {
                RouteKind::Step
            } else if let Some(ty) = outcome {
                RouteKind::Outcome(name.to_owned(), ty)
            } else {
                RouteKind::Unknown
            }
        }
        Env::Substep { parent, siblings } => {
            if siblings.iter().any(|sibling| sibling == name) {
                RouteKind::Sibling
            } else if parent.outcomes.iter().any(|clause| clause.name == name) {
                RouteKind::ParentArm
            } else if top_step || outcome.is_some() {
                RouteKind::Escapes(parent.name.clone())
            } else {
                RouteKind::Unknown
            }
        }
    }
}

fn sibling_span(env: &Env<'_>, name: &str) -> Option<Span> {
    let Env::Substep { parent, .. } = env else {
        return None;
    };
    parent.body.iter().find_map(|statement| match statement {
        Statement::SubStep(step) if step.name == name => Some(step.name_span),
        _ => None,
    })
}

/// Check one route: target existence per environment, payload contract
/// (constructed, picked up by name, or piped).
pub(super) fn check_route(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    target: &RouteTarget,
    env: &Env<'_>,
    piped: Option<Ty>,
) {
    let route = resolve_route(w, env, &target.name);
    match &route {
        RouteKind::Step => {
            w.ctx
                .semantic
                .reference(target.name_span, DeclarationKind::Step, &target.name);
        }
        RouteKind::Sibling => {
            w.ctx
                .semantic
                .reference_to(target.name_span, sibling_span(env, &target.name));
        }
        RouteKind::Outcome(name, ty) => {
            w.ctx
                .semantic
                .reference(target.name_span, DeclarationKind::Outcome, name);
            w.ctx.semantic.ty(target.name_span, &ty.to_string());
        }
        RouteKind::ParentArm | RouteKind::Escapes(_) | RouteKind::Unknown => {}
    }
    match route {
        RouteKind::Step | RouteKind::Sibling => {
            if piped.is_some() {
                w.err(
                    target.name_span,
                    format!(
                        "a piped route carries the piped value as the payload, but `{}` \
                         is a step and steps receive bindings, not payloads — bind the \
                         value (`-> name`) and `route {}` separately",
                        target.name, target.name
                    ),
                );
            } else if target.payload.is_some() {
                w.err(
                    target.name_span,
                    format!(
                        "routing to step `{}` carries no payload — steps receive \
                         bindings, not payloads",
                        target.name
                    ),
                );
            }
        }
        RouteKind::ParentArm => {
            if piped.is_some() {
                w.err(
                    target.name_span,
                    format!(
                        "a piped route carries the piped value as the payload, but `{}` \
                         is a parent outcome arm — the parent's own outcome clause \
                         carries the exit; bind the value (`-> name`) and `route {}` \
                         separately",
                        target.name, target.name
                    ),
                );
            } else if target.payload.is_some() {
                w.err(
                    target.name_span,
                    format!(
                        "routing to the parent outcome arm `{}` carries no payload — \
                         the parent's own outcome clause carries the exit",
                        target.name
                    ),
                );
            }
        }
        RouteKind::Outcome(name, ty) => {
            check_outcome_payload(w, view, target, &name, &ty, piped);
        }
        RouteKind::Escapes(parent) => {
            w.err(
                target.name_span,
                format!(
                    "route target `{}` is outside the parent step `{parent}` — an \
                     inner route may target a sibling substep or a parent outcome, \
                     never cross the parent boundary",
                    target.name
                ),
            );
        }
        RouteKind::Unknown => {
            w.err(
                target.name_span,
                format!(
                    "no step or workflow outcome named `{}` to route to",
                    target.name
                ),
            );
        }
    }
}

fn check_outcome_payload(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    target: &RouteTarget,
    name: &str,
    ty: &Ty,
    piped: Option<Ty>,
) {
    if let Some(piped_ty) = piped {
        if target.payload.is_some() {
            w.err(
                target.name_span,
                "a piped route carries the piped value — payload construction is \
                 not allowed here",
            );
        } else if !assignable(&piped_ty, ty, &w.ctx.types) {
            w.err(
                target.name_span,
                format!("the piped value is {piped_ty}, but outcome `{name}` carries {ty}"),
            );
        }
        return;
    }
    match &target.payload {
        Some(RoutePayload::Args(args)) => match resolve(ty, &w.ctx.types) {
            Ty::Record(record) => {
                let params: Vec<(String, Ty)> = record
                    .fields
                    .iter()
                    .map(|field| (field.name.clone(), field.ty.clone()))
                    .collect();
                check_args(
                    w,
                    view,
                    args,
                    &params,
                    &format!("outcome `{name}` (type {ty})"),
                    "field",
                    target.name_span,
                );
            }
            Ty::Unknown => {}
            other => {
                w.err(
                    target.name_span,
                    format!(
                        "outcome `{name}` carries {other} — a constructed payload \
                         needs a record-typed outcome"
                    ),
                );
            }
        },
        Some(RoutePayload::Value(value)) => {
            let value_ty = type_of(w, view, value);
            if !assignable(&value_ty, ty, &w.ctx.types) {
                w.err(
                    value.span(),
                    format!("the payload value is {value_ty}, but outcome `{name}` carries {ty}"),
                );
            }
        }
        None => match view_lookup(view, name) {
            Some(bound) => {
                if !assignable(&bound, ty, &w.ctx.types) {
                    w.err(
                        target.name_span,
                        format!(
                            "the binding `{name}` is {bound}, but outcome `{name}` \
                             carries {ty}"
                        ),
                    );
                }
            }
            None => {
                w.err(
                    target.name_span,
                    format!(
                        "`route {name}` needs a binding named `{name}` in scope to \
                         carry the payload"
                    ),
                );
            }
        },
    }
}

fn view_lookup(view: View<'_>, name: &str) -> Option<Ty> {
    if let Some((narrowed, ty)) = view.narrow
        && narrowed == name
    {
        return Some(ty.clone());
    }
    view.vars.get(name).map(|value| value.ty.clone())
}

/// Check a step's outcome clauses in written order.
pub(super) fn check_clauses(w: &mut Walker<'_, '_>, scope: &Scope, step: &Step, env: &Env<'_>) {
    // A step carrying `max … visits` reads the builtin `visits` counter (an
    // `Int`, 1-based) in its outcome guards — and only there.
    let with_visits;
    let scope = if let Some(max_visits) = &step.max_visits {
        let mut extended = scope.clone();
        extended.insert(
            "visits".to_owned(),
            ScopedTy {
                ty: Ty::Int,
                declaration: Some(max_visits.span),
            },
        );
        with_visits = extended;
        &with_visits
    } else {
        scope
    };
    // Loop exhaustion must be explicitly named (ruled 2026-07-11): a step
    // whose body contains a `loop` must declare conditional outcome clauses
    // covering the exhausted case; with zero clauses, `max` running out with
    // `until` still false would fall through indistinguishably from success.
    if step.outcomes.is_empty()
        && let Some(span) = first_loop_span(&step.body)
    {
        w.err(
            span,
            format!(
                "step `{}` contains a `loop` but declares no outcome clauses — an \
                 exhausted loop (`max` reached with `until` still false) would fall \
                 through indistinguishably from success; add conditional outcomes \
                 (`when`/`otherwise`) covering the exhausted case",
                step.name
            ),
        );
    }
    if let Some(first) = step.outcomes.first()
        && super::graph::body_ends_in_route(&step.body)
    {
        w.err(
            first.name_span,
            format!(
                "the outcome clauses of step `{}` can never fire — its body always \
                 routes away before outcomes are evaluated",
                step.name
            ),
        );
    }
    let count = step.outcomes.len();
    for (position, clause) in step.outcomes.iter().enumerate() {
        let mut narrow: Option<(String, Ty)> = None;
        match &clause.guard {
            Guard::When { expr, .. } => {
                let view = View {
                    vars: scope,
                    narrow: None,
                    accessor: None,
                };
                let ty = type_of(w, view, expr);
                if !matches!(resolve(&ty, &w.ctx.types), Ty::Bool | Ty::Unknown) {
                    w.err(
                        expr.span(),
                        format!("`when` needs a Bool guard, found {ty}"),
                    );
                }
                narrow = narrowed_binding(scope, expr);
            }
            Guard::Otherwise { span } => {
                if position + 1 != count {
                    w.err(
                        *span,
                        "`otherwise` is the complement of the preceding arms and must \
                         be last — arms after it can never fire",
                    );
                }
            }
        }
        let view = View {
            vars: scope,
            narrow: narrow.as_ref(),
            accessor: None,
        };
        check_route(w, view, &clause.route, env, None);
    }
    check_exhaustiveness(w, scope, step);
}

/// The span of the first `loop` in a step body, looking through fork
/// branches. Substeps are deliberately skipped: a substep owns its outcome
/// clauses, so a loop inside one answers to the substep's own coverage duty
/// (this function runs for substeps too, via their `check_clauses` call).
fn first_loop_span(statements: &[Statement]) -> Option<Span> {
    for statement in statements {
        match statement {
            Statement::Loop(looped) => return Some(looped.span),
            Statement::Fork(fork) => {
                if let Some(span) = first_loop_span(&fork.body) {
                    return Some(span);
                }
            }
            _ => {}
        }
    }
    None
}

/// `when x is present` narrows `x` from `T?` to `T` within its arm.
fn narrowed_binding(scope: &Scope, guard: &Expr) -> Option<(String, Ty)> {
    let Expr::Predicate {
        subject,
        kind: PredicateKind::Present,
        ..
    } = guard
    else {
        return None;
    };
    let Expr::Ref { name, .. } = subject.as_ref() else {
        return None;
    };
    match scope.get(name).map(|value| &value.ty) {
        Some(Ty::Optional(inner)) => Some((name.clone(), (**inner).clone())),
        _ => None,
    }
}
