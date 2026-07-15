//! Expression-reference collection shared by graph and MIR planning.

use std::collections::BTreeSet;

use crate::ast::Expr;

/// Names an expression references.
pub(crate) fn expr_refs(expr: &Expr, refs: &mut BTreeSet<String>) {
    match expr {
        Expr::String { .. }
        | Expr::RawString { .. }
        | Expr::Json { .. }
        | Expr::SchemaOf { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::Variant { .. }
        | Expr::Workflow { .. }
        | Expr::Accessor { .. } => {}
        Expr::List { items, .. } => {
            for item in items {
                expr_refs(item, refs);
            }
        }
        Expr::Ref { name, .. } => {
            refs.insert(name.clone());
        }
        Expr::Record { args, .. } => {
            for arg in args {
                expr_refs(&arg.value, refs);
            }
        }
        Expr::Field { base, .. } | Expr::Index { base, .. } => expr_refs(base, refs),
        Expr::Not { expr: inner, .. } => expr_refs(inner, refs),
        Expr::Binary { left, right, .. } => {
            expr_refs(left, refs);
            expr_refs(right, refs);
        }
        Expr::Predicate { subject, .. } => expr_refs(subject, refs),
        Expr::CollectionPredicate {
            collection,
            predicate,
            ..
        } => {
            expr_refs(collection, refs);
            expr_refs(predicate, refs);
        }
    }
}
