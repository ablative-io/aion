//! Reference-emitter lowering for quantified collection predicates.

use crate::Span;
use crate::ast::{Expr, Quantifier};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, expr_type, render_expr};
use super::types::GType;

pub(super) fn render_collection_predicate(
    emitter: &mut Emitter<'_>,
    span: Span,
    collection: &Expr,
    quantifier: Quantifier,
    predicate: &Expr,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    let collection_ty = emitter.env.resolve(&expr_type(emitter, collection, scope)?);
    let GType::List(element) = collection_ty else {
        return Err(EmitError::new(span, "collection predicate needs a list"));
    };
    let collection = render_expr(emitter, collection, scope, prelude)?;
    render_predicate_over(
        emitter,
        &collection,
        *element,
        quantifier,
        predicate,
        scope,
        prelude,
    )
}

pub(super) fn render_predicate_over(
    emitter: &mut Emitter<'_>,
    collection: &str,
    element: GType,
    quantifier: Quantifier,
    predicate: &Expr,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    let item = emitter.fresh_name(&format!("awl_item_{}", emitter.predicate_counter));
    emitter.predicate_counter += 1;
    let nested_scope = scope.with_accessor(item.clone(), element);
    let fallible = is_fallible(predicate);
    let mut nested_prelude = Vec::new();
    let predicate = render_expr(emitter, predicate, &nested_scope, &mut nested_prelude)?;
    let body = if nested_prelude.is_empty() {
        predicate.clone()
    } else {
        format!(
            "{{\n    {}\n    {predicate}\n  }}",
            nested_prelude.join("\n    ")
        )
    };
    emitter.flags.uses_list_module = true;
    if !fallible {
        let function = match quantifier {
            Quantifier::Any => "any",
            Quantifier::All => "all",
        };
        return Ok(format!(
            "list.{function}({collection}, fn({item}) {{ {body} }})"
        ));
    }
    let result = emitter.fresh_name(&format!(
        "awl_predicate_result_{}",
        emitter.predicate_counter
    ));
    let acc = emitter.fresh_name("awl_acc");
    let (initial, decisive) = match quantifier {
        Quantifier::Any => ("False", "True"),
        Quantifier::All => ("True", "False"),
    };
    prelude.push(format!(
        "use {result} <- result.try(list.try_fold({collection}, {initial}, fn({acc}, {item}) {{\n  case {acc} {{\n    {decisive} -> Ok({decisive})\n    _ -> {{\n      {}\n      Ok({predicate})\n    }}\n  }}\n}}))",
        nested_prelude.join("\n      ")
    ));
    Ok(result)
}

pub(crate) fn is_fallible(expr: &Expr) -> bool {
    match expr {
        Expr::Field { base, name, .. } => {
            (name == "id" && matches!(base.as_ref(), Expr::Workflow { .. })) || is_fallible(base)
        }
        Expr::Index { .. } => true,
        Expr::List { items, .. } => items.iter().any(is_fallible),
        Expr::Record { args, .. } => args.iter().any(|arg| is_fallible(&arg.value)),
        Expr::Not { expr, .. } | Expr::Predicate { subject: expr, .. } => is_fallible(expr),
        Expr::Binary { left, right, .. } => is_fallible(left) || is_fallible(right),
        Expr::CollectionPredicate {
            collection,
            predicate,
            ..
        } => is_fallible(collection) || is_fallible(predicate),
        Expr::String { .. }
        | Expr::RawString { .. }
        | Expr::Json { .. }
        | Expr::SchemaOf { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::Ref { .. }
        | Expr::Workflow { .. }
        | Expr::Variant { .. }
        | Expr::Accessor { .. } => false,
    }
}
