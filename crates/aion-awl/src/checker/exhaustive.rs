//! Outcome-clause exhaustiveness: the generic `otherwise` duty and the
//! enum-subject totality pattern (every arm compares one subject against a
//! bare variant; the diagnostic names the uncovered variants).

use crate::ast::{BinaryOp, Expr, Guard, Step};

use super::exprs::{View, type_of};
use super::types::{Ty, resolve};
use super::walk::{Scope, Walker};

pub(super) fn check_exhaustiveness(w: &mut Walker<'_, '_>, scope: &Scope, step: &Step) {
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
fn enum_totality_gap(w: &mut Walker<'_, '_>, scope: &Scope, step: &Step) -> Option<TotalityGap> {
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
        accessor: None,
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
