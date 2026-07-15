//! The `const` pass: name hygiene, compile-time value typing, duplicate and
//! collision rejection, and cycle detection over const-to-const references.
//!
//! A `const` value is restricted to compile-time forms — literals (including
//! raw strings and `json { … }` bodies), `schema of Type`, list literals of
//! these, `+` concatenations of these, and references to other consts — so
//! the fold pass can always reduce it to one literal before lowering.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::Span;
use crate::ast::{BinaryOp, ConstDecl, Expr};
use crate::spanned::Spanned;

use super::context::{BUILTIN_TYPES, ConstInfo, Ctx};
use super::decls::is_snake_case;
use super::types::{Ty, assignable, resolve};

/// The resolution state of one const during cycle-aware typing.
enum State {
    /// Resolution entered but not finished: a reference back here is a cycle.
    Visiting,
    /// Resolution finished with this type.
    Done(Ty),
}

/// Run the const pass, populating `ctx.consts`.
pub(super) fn run(ctx: &mut Ctx<'_>) {
    let doc = ctx.doc;
    let mut firsts: BTreeMap<&str, &ConstDecl> = BTreeMap::new();
    for decl in &doc.consts {
        if !is_snake_case(&decl.name) {
            ctx.error(
                decl.name_span,
                format!(
                    "const name `{}` must be snake_case ([a-z][a-z0-9_]*)",
                    decl.name
                ),
            );
        }
        if ctx.inputs.contains_key(&decl.name) || ctx.signals.contains_key(&decl.name) {
            ctx.error(
                decl.name_span,
                format!(
                    "const `{}` collides with the workflow input or signal of the same \
                     name — const names must be unambiguous everywhere an expression \
                     is legal",
                    decl.name
                ),
            );
        }
        if firsts.contains_key(decl.name.as_str()) {
            ctx.error(
                decl.name_span,
                format!("duplicate const declaration `{}`", decl.name),
            );
            continue;
        }
        firsts.insert(&decl.name, decl);
    }
    let mut states: BTreeMap<String, State> = BTreeMap::new();
    for decl in &doc.consts {
        let ty = resolve_const(ctx, &firsts, &decl.name, &mut states);
        ctx.semantic.ty(decl.name_span, &ty.to_string());
        // The first declaration wins under duplicates, mirroring types.
        ctx.consts.entry(decl.name.clone()).or_insert(ConstInfo {
            ty,
            name_span: decl.name_span,
        });
    }
}

/// Resolve one const's type, memoized; `Visiting` marks the active DFS path
/// so a reference back into it is reported as a cycle exactly once.
fn resolve_const(
    ctx: &mut Ctx<'_>,
    firsts: &BTreeMap<&str, &ConstDecl>,
    name: &str,
    states: &mut BTreeMap<String, State>,
) -> Ty {
    match states.get(name) {
        Some(State::Done(ty)) => return ty.clone(),
        Some(State::Visiting) => return Ty::Unknown,
        None => {}
    }
    let Some(decl) = firsts.get(name) else {
        return Ty::Unknown;
    };
    states.insert(name.to_owned(), State::Visiting);
    let ty = value_ty(ctx, firsts, &decl.value, states);
    states.insert(name.to_owned(), State::Done(ty.clone()));
    ty
}

/// Type one compile-time const value expression, reporting every defect.
fn value_ty(
    ctx: &mut Ctx<'_>,
    firsts: &BTreeMap<&str, &ConstDecl>,
    expr: &Expr,
    states: &mut BTreeMap<String, State>,
) -> Ty {
    match expr {
        Expr::String { .. } | Expr::RawString { .. } => Ty::Str,
        Expr::Int { .. } => Ty::Int,
        Expr::Float { .. } => Ty::Float,
        Expr::Bool { .. } => Ty::Bool,
        Expr::Duration(_) => Ty::Duration,
        Expr::Json {
            body, body_span, ..
        } => {
            if let Some((span, message)) = json_literal_error(body, *body_span) {
                ctx.error(span, message);
            }
            Ty::Str
        }
        Expr::SchemaOf {
            name, name_span, ..
        } => {
            check_schema_of_target(ctx, name, *name_span);
            Ty::Str
        }
        Expr::List { items, .. } => {
            let mut element = Ty::Unknown;
            for item in items {
                let ty = value_ty(ctx, firsts, item, states);
                if matches!(element, Ty::Unknown) {
                    element = ty;
                } else if !matches!(ty, Ty::Unknown) && !assignable(&ty, &element, &ctx.types) {
                    ctx.error(
                        item.span(),
                        format!("list items must share one type: found {element} and {ty}"),
                    );
                }
            }
            Ty::List(Rc::new(element))
        }
        Expr::Binary {
            span,
            left,
            op: BinaryOp::Concat,
            right,
        } => {
            let left_ty = value_ty(ctx, firsts, left, states);
            let right_ty = value_ty(ctx, firsts, right, states);
            let joinable = |ty: &Ty| matches!(resolve(ty, &ctx.types), Ty::Str | Ty::Unknown);
            if !joinable(&left_ty) || !joinable(&right_ty) {
                ctx.error(
                    *span,
                    format!("`+` in a `const` joins strings only (found {left_ty} and {right_ty})"),
                );
            }
            Ty::Str
        }
        Expr::Ref { span, name } => const_ref_ty(ctx, firsts, *span, name, states),
        other => {
            ctx.error(
                other.span(),
                "a `const` value must be compile-time: a literal (including raw \
                 strings and `json { … }`), `schema of <Type>`, a list of these, \
                 `+` concatenations of these, or another const",
            );
            Ty::Unknown
        }
    }
}

/// Type a reference inside a const value: only other consts are legal.
fn const_ref_ty(
    ctx: &mut Ctx<'_>,
    firsts: &BTreeMap<&str, &ConstDecl>,
    span: Span,
    name: &str,
    states: &mut BTreeMap<String, State>,
) -> Ty {
    if firsts.contains_key(name) {
        if matches!(states.get(name), Some(State::Visiting)) {
            ctx.error(
                span,
                format!("const `{name}` is defined in terms of itself — const values cannot cycle"),
            );
            return Ty::Unknown;
        }
        let ty = resolve_const(ctx, firsts, name, states);
        if let Some(info) = ctx.consts.get(name) {
            let declaration = info.name_span;
            ctx.semantic.reference_to(span, Some(declaration));
        } else if let Some(decl) = firsts.get(name) {
            let declaration = decl.name_span;
            ctx.semantic.reference_to(span, Some(declaration));
        }
        return ty;
    }
    if ctx.inputs.contains_key(name) || ctx.signals.contains_key(name) {
        ctx.error(
            span,
            format!(
                "a `const` value is compile-time — `{name}` is a workflow input or \
                 signal, not a const"
            ),
        );
        return Ty::Unknown;
    }
    ctx.error(
        span,
        format!("unknown const `{name}` — a `const` value may reference only other consts"),
    );
    Ty::Unknown
}

/// Report an invalid `json { … }` literal body, with the span pointing into
/// the body at the offending position. Returns `None` when the body is
/// valid JSON.
pub(super) fn json_literal_error(body: &str, body_span: Span) -> Option<(Span, String)> {
    let Err(error) = serde_json::from_str::<serde_json::Value>(body) else {
        return None;
    };
    let (span, detail) = crate::jsontext::json_error_anchor(body, body_span, &error);
    Some((
        span,
        format!("`json {{ … }}` literal body is not valid JSON: {detail}"),
    ))
}

/// Whether `schema of <name>` names a resolvable type, reporting the error
/// when it does not.
pub(super) fn check_schema_of_target(ctx: &mut Ctx<'_>, name: &str, name_span: Span) {
    if BUILTIN_TYPES.contains(&name) || ctx.type_names.contains(name) {
        ctx.semantic
            .reference(name_span, crate::semantic::DeclarationKind::Type, name);
        return;
    }
    ctx.error(name_span, format!("unknown type `{name}`"));
}
