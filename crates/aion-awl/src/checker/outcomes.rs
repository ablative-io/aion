//! Outcome-clause checking: guard typing, guard-dependent optionality
//! narrowing, route-target resolution (steps, workflow outcomes, substep
//! siblings, parent arms), payload contracts, `otherwise` placement, and
//! exhaustiveness (including enum-subject totality).

use std::collections::BTreeMap;

use crate::ast::{BinaryOp, Expr, Guard, PredicateKind, RouteTarget, Step};
use crate::spanned::Spanned;

use super::exprs::{View, check_args, type_of};
use super::types::{Ty, assignable, resolve};
use super::walk::Walker;

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
    let top_step = w.ctx.doc.steps.iter().any(|step| step.name == name);
    let outcome = w.ctx.outcome_types.get(name).cloned();
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

/// Check one route: target existence per environment, payload contract
/// (constructed, picked up by name, or piped).
pub(super) fn check_route(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    target: &RouteTarget,
    env: &Env<'_>,
    piped: Option<Ty>,
) {
    match resolve_route(w, env, &target.name) {
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
        Some(args) => match resolve(ty, &w.ctx.types) {
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
    view.vars.get(name).cloned()
}

/// Check a step's outcome clauses in written order.
pub(super) fn check_clauses(
    w: &mut Walker<'_, '_>,
    scope: &BTreeMap<String, Ty>,
    step: &Step,
    env: &Env<'_>,
) {
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
        };
        check_route(w, view, &clause.route, env, None);
    }
    check_exhaustiveness(w, scope, step);
}

/// `when x is present` narrows `x` from `T?` to `T` within its arm.
fn narrowed_binding(scope: &BTreeMap<String, Ty>, guard: &Expr) -> Option<(String, Ty)> {
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
    match scope.get(name) {
        Some(Ty::Optional(inner)) => Some((name.clone(), (**inner).clone())),
        _ => None,
    }
}

fn check_exhaustiveness(w: &mut Walker<'_, '_>, scope: &BTreeMap<String, Ty>, step: &Step) {
    if step.outcomes.is_empty() {
        return;
    }
    if step
        .outcomes
        .iter()
        .any(|clause| matches!(clause.guard, Guard::Otherwise { .. }))
    {
        return;
    }
    if let Some(missing) = enum_totality_gap(w, scope, step) {
        if missing.uncovered.is_empty() {
            return;
        }
        w.err(
            step.name_span,
            format!(
                "step `{}` has conditional outcomes that are not exhaustive: enum \
                 variant{} {} of `{}` {} never covered — add an arm or `otherwise`",
                step.name,
                if missing.uncovered.len() == 1 {
                    ""
                } else {
                    "s"
                },
                missing
                    .uncovered
                    .iter()
                    .map(|variant| format!("`{variant}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                missing.enum_name,
                if missing.uncovered.len() == 1 {
                    "is"
                } else {
                    "are"
                },
            ),
        );
        return;
    }
    w.err(
        step.name_span,
        format!(
            "step `{}` has conditional outcomes that are not exhaustive — add an \
             `otherwise` arm so nothing (including an exhausted loop) falls off the end",
            step.name
        ),
    );
}

struct TotalityGap {
    enum_name: String,
    uncovered: Vec<String>,
}

/// Detect the enum-subject totality pattern: every arm compares the same
/// subject expression against a bare variant. Returns `None` when the arms
/// do not fit the pattern (the generic diagnostic applies).
fn enum_totality_gap(
    w: &mut Walker<'_, '_>,
    scope: &BTreeMap<String, Ty>,
    step: &Step,
) -> Option<TotalityGap> {
    let mut subject_key: Option<String> = None;
    let mut subject_expr: Option<&Expr> = None;
    let mut covered: Vec<String> = Vec::new();
    for clause in &step.outcomes {
        let Guard::When { expr, .. } = &clause.guard else {
            return None;
        };
        let Expr::Binary {
            left,
            op: BinaryOp::Eq,
            right,
            ..
        } = expr
        else {
            return None;
        };
        let (subject, variant) = match (left.as_ref(), right.as_ref()) {
            (Expr::Variant { name, .. }, other) | (other, Expr::Variant { name, .. }) => {
                (other, name.clone())
            }
            _ => return None,
        };
        let key = expr_key(subject)?;
        match &subject_key {
            None => {
                subject_key = Some(key);
                subject_expr = Some(subject);
            }
            Some(existing) if *existing == key => {}
            Some(_) => return None,
        }
        covered.push(variant);
    }
    let subject = subject_expr?;
    let view = View {
        vars: scope,
        narrow: None,
    };
    let subject_ty = w.silently(|w| type_of(w, view, subject));
    let Ty::Enum(spec) = resolve(&subject_ty, &w.ctx.types) else {
        return None;
    };
    let uncovered: Vec<String> = spec
        .variants
        .iter()
        .filter(|variant| !covered.contains(variant))
        .cloned()
        .collect();
    Some(TotalityGap {
        enum_name: spec.name.clone().unwrap_or_else(|| "the enum".to_owned()),
        uncovered,
    })
}

/// A canonical key for a subject expression: a reference or a field path.
fn expr_key(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ref { name, .. } => Some(name.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{name}", expr_key(base)?)),
        _ => None,
    }
}
