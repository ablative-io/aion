//! Operator typing: boolean connectives with guard narrowing, string
//! concatenation, comparisons, equality (including bare enum variants), and
//! `is` predicates — split from the expression walker for the 500-line law.

use crate::Span;
use crate::ast::{BinaryOp, Expr, PredicateKind};
use crate::spanned::Spanned;

use super::exprs::{View, type_of};
use super::types::{Ty, equality_comparable, resolve};
use super::walk::Walker;

pub(super) fn is_bool(ty: &Ty, w: &Walker<'_, '_>) -> bool {
    matches!(resolve(ty, &w.ctx.types), Ty::Bool | Ty::Unknown)
}

pub(super) fn type_binary(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    span: Span,
    left: &Expr,
    op: BinaryOp,
    right: &Expr,
) -> Ty {
    match op {
        BinaryOp::And | BinaryOp::Or => {
            let word = if matches!(op, BinaryOp::And) {
                "and"
            } else {
                "or"
            };
            let left_ty = type_of(w, view, left);
            if !is_bool(&left_ty, w) {
                w.err(
                    left.span(),
                    format!("`{word}` needs Bool operands, found {left_ty}"),
                );
            }
            let narrowed_kind = match op {
                BinaryOp::And => PredicateKind::Present,
                BinaryOp::Or => PredicateKind::Absent,
                _ => unreachable!(),
            };
            let narrow = match left {
                Expr::Predicate { subject, kind, .. } if *kind == narrowed_kind => {
                    match subject.as_ref() {
                        Expr::Ref { name, .. } => view.get(name).and_then(|ty| {
                            let Ty::Optional(inner) = resolve(&ty, &w.ctx.types) else {
                                return None;
                            };
                            Some((name.clone(), inner.as_ref().clone()))
                        }),
                        _ => None,
                    }
                }
                _ => None,
            };
            let right_view = View {
                vars: view.vars,
                narrow: narrow.as_ref().or(view.narrow),
                accessor: view.accessor,
            };
            let right_ty = type_of(w, right_view, right);
            if !is_bool(&right_ty, w) {
                w.err(
                    right.span(),
                    format!("`{word}` needs Bool operands, found {right_ty}"),
                );
            }
            Ty::Bool
        }
        BinaryOp::Concat => {
            let left_ty = type_of(w, view, left);
            let right_ty = type_of(w, view, right);
            let joinable = |ty: &Ty| matches!(resolve(ty, &w.ctx.types), Ty::Str | Ty::Unknown);
            if !joinable(&left_ty) || !joinable(&right_ty) {
                w.err(
                    span,
                    format!(
                        "`+` joins strings only — arithmetic is not in the language \
                         (found {left_ty} and {right_ty})"
                    ),
                );
            }
            Ty::Str
        }
        BinaryOp::Eq | BinaryOp::Ne => {
            let symbol = if matches!(op, BinaryOp::Eq) {
                "=="
            } else {
                "!="
            };
            type_equality(w, view, span, left, right, symbol);
            Ty::Bool
        }
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let symbol = match op {
                BinaryOp::Lt => "<",
                BinaryOp::Le => "<=",
                BinaryOp::Gt => ">",
                _ => ">=",
            };
            let left_ty = resolve(&type_of(w, view, left), &w.ctx.types);
            let right_ty = resolve(&type_of(w, view, right), &w.ctx.types);
            let ordered = matches!(
                (&left_ty, &right_ty),
                (Ty::Int, Ty::Int) | (Ty::Float, Ty::Float) | (Ty::Unknown, _) | (_, Ty::Unknown)
            );
            if !ordered {
                w.err(
                    span,
                    format!(
                        "`{symbol}` needs matching numeric operands, found {left_ty} \
                         and {right_ty}"
                    ),
                );
            }
            Ty::Bool
        }
    }
}

fn type_equality(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    span: Span,
    left: &Expr,
    right: &Expr,
    symbol: &str,
) {
    let variant_side = match (left, right) {
        (Expr::Variant { span, name }, other) | (other, Expr::Variant { span, name }) => {
            Some((*span, name.clone(), other))
        }
        _ => None,
    };
    if let Some((variant_span, variant, subject)) = variant_side {
        let subject_ty = type_of(w, view, subject);
        match resolve(&subject_ty, &w.ctx.types) {
            Ty::Enum(spec) => {
                if !spec.variants.contains(&variant) {
                    let name = spec.name.clone().unwrap_or_else(|| "the enum".to_owned());
                    w.err(
                        variant_span,
                        format!("enum `{name}` has no variant `{variant}`"),
                    );
                }
            }
            Ty::Unknown => {}
            other => {
                w.err(
                    variant_span,
                    format!(
                        "`{symbol}` compares variant `{variant}` against {other}, \
                         which is not an enum"
                    ),
                );
            }
        }
        return;
    }
    let left_ty = type_of(w, view, left);
    let right_ty = type_of(w, view, right);
    if !equality_comparable(&left_ty, &right_ty, &w.ctx.types) {
        w.err(
            span,
            format!("`{symbol}` needs matching operands, found {left_ty} and {right_ty}"),
        );
    }
}

pub(super) fn type_predicate(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    span: Span,
    subject: &Expr,
    kind: PredicateKind,
) -> Ty {
    let subject_ty = type_of(w, view, subject);
    let resolved = resolve(&subject_ty, &w.ctx.types);
    match kind {
        PredicateKind::Empty => {
            if !matches!(resolved, Ty::List(_) | Ty::Unknown) {
                w.err(
                    span,
                    format!("`is empty` applies to lists, found {subject_ty}"),
                );
            }
        }
        PredicateKind::Present | PredicateKind::Absent => {
            let word = if matches!(kind, PredicateKind::Present) {
                "is present"
            } else {
                "is absent"
            };
            if !matches!(resolved, Ty::Optional(_) | Ty::Unknown) {
                w.err(
                    span,
                    format!("`{word}` applies to optional (`?`) values, found plain {subject_ty}"),
                );
            }
        }
    }
    Ty::Bool
}
