//! Expression typing: literals, references, field access (with the
//! guard-dependent optionality rule), record construction, comparisons,
//! boolean operators, string concatenation, indexing, and `is` predicates.

use crate::Span;
use crate::ast::Expr;
use crate::spanned::Spanned;

use super::operators::{is_bool, type_binary, type_predicate};
use super::types::{Ty, assignable, resolve};
use super::walk::{Scope, Walker};

/// A read-only view of the bindings in scope, with at most one
/// guard-narrowed name (`when x is present` makes `x` a `T` in its arm).
#[derive(Clone, Copy)]
pub(super) struct View<'s> {
    /// Bindings in scope: name → type.
    pub(super) vars: &'s Scope,
    /// The one narrowed binding, if the surrounding arm narrows.
    pub(super) narrow: Option<&'s (String, Ty)>,
    /// Element type bound to `.field` inside an `any`/`all` predicate.
    pub(super) accessor: Option<&'s Ty>,
}

impl View<'_> {
    /// A top-level expression view with no narrowing or predicate item.
    pub(super) const fn plain(vars: &Scope) -> View<'_> {
        View {
            vars,
            narrow: None,
            accessor: None,
        }
    }

    pub(super) fn get(&self, name: &str) -> Option<Ty> {
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
        Expr::String { .. } | Expr::RawString { .. } => Ty::Str,
        Expr::Json {
            body, body_span, ..
        } => type_json(w, body, *body_span),
        Expr::SchemaOf {
            name, name_span, ..
        } => type_schema_of(w, name, *name_span),
        Expr::Int { .. } => Ty::Int,
        Expr::Float { .. } => Ty::Float,
        Expr::Bool { .. } => Ty::Bool,
        Expr::Duration(_) => Ty::Duration,
        Expr::List { items, .. } => type_list(w, view, items),
        Expr::Ref { span, name } => type_ref(w, view, *span, name),
        Expr::Workflow { span } => {
            w.err(
                *span,
                "`workflow` is a builtin namespace, not a value — use `workflow.id`",
            );
            Ty::Unknown
        }
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
        } => type_field(w, view, base, name, *name_span),
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
            if let Some(element) = view.accessor {
                field_access(w, element, None, name, *span)
            } else {
                w.err(
                    *span,
                    format!("a bare `.{name}` accessor is only a combinator argument"),
                );
                Ty::Unknown
            }
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
        Expr::CollectionPredicate {
            collection,
            predicate,
            quantifier,
            ..
        } => super::collections::type_collection_predicate(
            w,
            view,
            collection,
            predicate,
            *quantifier,
        ),
    }
}

/// Type a `json { … }` literal: the value is its verbatim body text, and
/// the body must be valid JSON (the diagnostic points into the body).
fn type_json(w: &mut Walker<'_, '_>, body: &str, body_span: Span) -> Ty {
    if let Some((span, message)) = super::consts::json_literal_error(body, body_span) {
        w.err(span, message);
    }
    Ty::Str
}

/// Type a `schema of <Type>` expression: a compile-time `String` whose type
/// operand must resolve.
fn type_schema_of(w: &mut Walker<'_, '_>, name: &str, name_span: Span) -> Ty {
    if w.emit {
        super::consts::check_schema_of_target(w.ctx, name, name_span);
    }
    Ty::Str
}

fn type_field(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    base: &Expr,
    name: &str,
    name_span: Span,
) -> Ty {
    if matches!(base, Expr::Workflow { .. }) {
        if name == "id" {
            return Ty::Str;
        }
        w.err(
            name_span,
            format!("unknown workflow builtin `workflow.{name}` — only `workflow.id` is available"),
        );
        return Ty::Unknown;
    }
    let base_ty = type_of(w, view, base);
    field_access(w, &base_ty, Some(base), name, name_span)
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
    if let Some(info) = w.ctx.consts.get(name) {
        let ty = info.ty.clone();
        let declaration = info.name_span;
        if w.emit {
            w.ctx.semantic.reference_to(span, Some(declaration));
        }
        return ty;
    }
    if name == "visits" {
        w.err(
            span,
            "`visits` is the builtin visit counter — it is readable only in the \
             outcome guards of the step that declares `max … visits`",
        );
    } else if w.prior.contains_key(name) {
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
            super::args::check_args(
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
