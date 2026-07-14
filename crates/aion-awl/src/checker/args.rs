//! Named argument and expected-value checking for calls, records, and routes.

use crate::Span;
use crate::ast::Expr;
use crate::spanned::Spanned;

use super::exprs::{View, type_of};
use super::types::{Ty, assignable, resolve};
use super::walk::Walker;

/// Check a value expression against an expected type, resolving a bare enum
/// variant against the expected enum.
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

/// Check named arguments against a parameter or field list.
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
        if !matches!(expected, Ty::Optional(_)) && !args.iter().any(|arg| arg.name == *name) {
            w.err(anchor, format!("missing {term} `{name}` in {owner}"));
        }
    }
}
