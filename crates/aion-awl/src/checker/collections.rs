//! Type checking for quantified collection predicates.

use crate::ast::{Expr, Quantifier};
use crate::spanned::Spanned;

use super::exprs::{View, type_of};
use super::types::{Ty, resolve};
use super::walk::Walker;

pub(super) fn type_collection_predicate(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    collection: &Expr,
    predicate: &Expr,
    quantifier: Quantifier,
) -> Ty {
    let noun = match quantifier {
        Quantifier::Any => "any",
        Quantifier::All => "all",
    };
    let collection_ty = type_of(w, view, collection);
    let element = match resolve(&collection_ty, &w.ctx.types) {
        Ty::List(inner) => (*inner).clone(),
        Ty::Unknown => Ty::Unknown,
        other => {
            w.err(
                collection.span(),
                format!("`{noun}` needs a List collection, found {other}"),
            );
            Ty::Unknown
        }
    };
    check_element_predicate(w, view, predicate, &element, noun);
    Ty::Bool
}

pub(super) fn check_element_predicate(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    predicate: &Expr,
    element: &Ty,
    noun: &str,
) {
    let predicate_view = View {
        vars: view.vars,
        narrow: view.narrow,
        accessor: Some(element),
    };
    let predicate_ty = type_of(w, predicate_view, predicate);
    if !matches!(resolve(&predicate_ty, &w.ctx.types), Ty::Bool | Ty::Unknown) {
        w.err(
            predicate.span(),
            format!("`{noun}` predicate must be Bool, found {predicate_ty}"),
        );
    }
}
