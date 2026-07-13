//! Expression typing: literals, references, field access (with the
//! guard-dependent optionality rule), record construction, comparisons,
//! boolean operators, string concatenation, indexing, and `is` predicates.

use crate::Span;
use crate::ast::{BinaryOp, Expr, PredicateKind};
use crate::spanned::Spanned;

use super::types::{Ty, assignable, equality_comparable, resolve};
use super::walk::{Scope, Walker};

/// A read-only view of the bindings in scope, with at most one
/// guard-narrowed name (`when x is present` makes `x` a `T` in its arm).
#[derive(Clone, Copy)]
pub(super) struct View<'s> {
    /// Bindings in scope: name → type.
    pub(super) vars: &'s Scope,
    /// The one narrowed binding, if the surrounding arm narrows.
    pub(super) narrow: Option<&'s (String, Ty)>,
}

impl View<'_> {
    fn get(&self, name: &str) -> Option<Ty> {
        if let Some((narrowed, ty)) = self.narrow
            && narrowed == name
        {
            return Some(ty.clone());
        }
        self.vars.get(name).map(|value| value.ty.clone())
    }
}

/// Type an expression, reporting every defect found inside it.
pub(super) fn type_of(w: &mut Walker<'_, '_>, view: View<'_>, expr: &Expr) -> Ty {
    let ty = type_of_inner(w, view, expr);
    if w.emit {
        w.ctx.semantic.ty(expr.span(), &ty.to_string());
    }
    ty
}

fn type_of_inner(w: &mut Walker<'_, '_>, view: View<'_>, expr: &Expr) -> Ty {
    match expr {
        Expr::String { .. } => Ty::Str,
        Expr::Int { .. } => Ty::Int,
        Expr::Float { .. } => Ty::Float,
        Expr::Bool { .. } => Ty::Bool,
        Expr::Duration(_) => Ty::Duration,
        Expr::List { items, .. } => type_list(w, view, items),
        Expr::Ref { span, name } => type_ref(w, view, *span, name),
        Expr::Variant { span, name } => {
            w.err(
                *span,
                format!(
                    "bare enum variant `{name}` is only usable compared against an \
                     enum-typed value"
                ),
            );
            Ty::Unknown
        }
        Expr::Record {
            name,
            name_span,
            args,
            ..
        } => type_record(w, view, name, *name_span, args),
        Expr::Field {
            base,
            name,
            name_span,
            ..
        } => {
            let base_ty = type_of(w, view, base);
            field_access(w, &base_ty, Some(base), name, *name_span)
        }
        Expr::Index { span, base, .. } => {
            let base_ty = type_of(w, view, base);
            match resolve(&base_ty, &w.ctx.types) {
                Ty::List(inner) => (*inner).clone(),
                Ty::Unknown => Ty::Unknown,
                other => {
                    w.err(
                        *span,
                        format!("only lists can be indexed — this value is {other}"),
                    );
                    Ty::Unknown
                }
            }
        }
        Expr::Accessor { span, name } => {
            w.err(
                *span,
                format!("a bare `.{name}` accessor is only a combinator argument"),
            );
            Ty::Unknown
        }
        Expr::Not { span, expr: inner } => {
            let ty = type_of(w, view, inner);
            if !is_bool(&ty, w) {
                w.err(*span, format!("`not` needs a Bool operand, found {ty}"));
            }
            Ty::Bool
        }
        Expr::Binary {
            span,
            left,
            op,
            right,
        } => type_binary(w, view, *span, left, *op, right),
        Expr::Predicate {
            span,
            subject,
            kind,
        } => type_predicate(w, view, *span, subject, *kind),
    }
}

fn is_bool(ty: &Ty, w: &Walker<'_, '_>) -> bool {
    matches!(resolve(ty, &w.ctx.types), Ty::Bool | Ty::Unknown)
}

fn type_list(w: &mut Walker<'_, '_>, view: View<'_>, items: &[Expr]) -> Ty {
    let mut element = Ty::Unknown;
    for item in items {
        let ty = type_of(w, view, item);
        if matches!(element, Ty::Unknown) {
            element = ty;
        } else if !matches!(ty, Ty::Unknown) && !assignable(&ty, &element, &w.ctx.types) {
            w.err(
                item.span(),
                format!("list items must share one type: found {element} and {ty}"),
            );
        }
    }
    Ty::List(std::rc::Rc::new(element))
}

fn type_ref(w: &mut Walker<'_, '_>, view: View<'_>, span: Span, name: &str) -> Ty {
    if name == "null" {
        w.err(
            span,
            "`null` does not exist in AWL — absence is expressed by omitting an \
             optional (`?`) field, never by null",
        );
        return Ty::Unknown;
    }
    if let Some(ty) = view.get(name) {
        if w.emit {
            let declaration = view.vars.get(name).and_then(|value| value.declaration);
            w.ctx.semantic.reference_to(span, declaration);
        }
        return ty;
    }
    if w.prior.contains_key(name) {
        w.err(
            span,
            format!(
                "`{name}` is bound on some path but not guaranteed on every path into \
                 this step — bindings are readable only where guaranteed"
            ),
        );
    } else {
        w.err(
            span,
            format!("unknown name `{name}` — nothing binds it here"),
        );
    }
    Ty::Unknown
}

fn type_record(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    name: &str,
    name_span: Span,
    args: &[crate::ast::Arg],
) -> Ty {
    let Some(definition) = w.ctx.types.get(name).cloned() else {
        w.err(name_span, format!("unknown type `{name}`"));
        return Ty::Unknown;
    };
    w.ctx
        .semantic
        .reference(name_span, crate::semantic::DeclarationKind::Type, name);
    match resolve(&definition, &w.ctx.types) {
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
                &format!("type `{name}`"),
                "field",
                name_span,
            );
            Ty::Named(name.to_owned())
        }
        Ty::Unknown => Ty::Unknown,
        other => {
            w.err(
                name_span,
                format!("`{name}` is {other} and has no construction form"),
            );
            Ty::Unknown
        }
    }
}

/// Type a `.field` access on a value, applying the guard-dependent
/// optionality rule: reading through a `T?` requires an `is present` guard.
pub(super) fn field_access(
    w: &mut Walker<'_, '_>,
    base_ty: &Ty,
    base: Option<&Expr>,
    name: &str,
    name_span: Span,
) -> Ty {
    match resolve(base_ty, &w.ctx.types) {
        Ty::Unknown => Ty::Unknown,
        Ty::Optional(inner) => {
            let subject = match base {
                Some(Expr::Ref {
                    name: base_name, ..
                }) => format!("`{base_name}`"),
                _ => "this value".to_owned(),
            };
            let span = base.map_or(name_span, Spanned::span);
            w.err(
                span,
                format!(
                    "{subject} may be absent ({inner}?) — guard with `is present` \
                     before reading `.{name}`"
                ),
            );
            Ty::Unknown
        }
        Ty::Record(record) => {
            if let Some(field) = record.field(name) {
                w.ctx.semantic.reference_to(name_span, field.declaration);
                w.ctx.semantic.ty(name_span, &field.ty.to_string());
                field.ty.clone()
            } else {
                let display = base_ty.to_string();
                w.err(name_span, format!("`{display}` has no field `{name}`"));
                Ty::Unknown
            }
        }
        other => {
            w.err(
                name_span,
                format!("`{other}` has no fields — field access needs an object"),
            );
            Ty::Unknown
        }
    }
}

fn type_binary(
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

fn type_predicate(
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

/// Check a value expression against an expected type, resolving bare enum
/// variants against an expected enum.
pub(super) fn check_value(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    expr: &Expr,
    expected: &Ty,
    describe: impl FnOnce(&Ty) -> String,
) {
    if let Expr::Variant { span, name } = expr {
        match resolve(expected, &w.ctx.types) {
            Ty::Enum(spec) => {
                if !spec.variants.contains(name) {
                    let enum_name = spec.name.clone().unwrap_or_else(|| "the enum".to_owned());
                    w.err(*span, format!("enum `{enum_name}` has no variant `{name}`"));
                }
                return;
            }
            Ty::Unknown => return,
            _ => {}
        }
    }
    let actual = type_of(w, view, expr);
    if !assignable(&actual, expected, &w.ctx.types) {
        w.err(expr.span(), describe(&actual));
    }
}

/// Check named arguments against a parameter/field list: exact names,
/// no duplicates, no omissions (optional-typed entries may be omitted),
/// every value assignable.
pub(super) fn check_args(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    args: &[crate::ast::Arg],
    params: &[(String, Ty)],
    owner: &str,
    term: &str,
    anchor: Span,
) {
    let mut seen: Vec<&str> = Vec::new();
    for arg in args {
        if seen.contains(&arg.name.as_str()) {
            w.err(arg.name_span, format!("duplicate {term} `{}`", arg.name));
            continue;
        }
        seen.push(arg.name.as_str());
        let Some((name, expected)) = params.iter().find(|(name, _)| *name == arg.name) else {
            w.err(
                arg.name_span,
                format!("{owner} has no {term} `{}`", arg.name),
            );
            continue;
        };
        let (name, owner) = (name.clone(), owner.to_owned());
        let expected = expected.clone();
        check_value(w, view, &arg.value, &expected, |actual| {
            format!("{term} `{name}` of {owner} expects {expected}, found {actual}")
        });
    }
    for (name, expected) in params {
        if matches!(expected, Ty::Optional(_)) {
            continue;
        }
        if !args.iter().any(|arg| arg.name == *name) {
            w.err(anchor, format!("missing {term} `{name}` in {owner}"));
        }
    }
}
