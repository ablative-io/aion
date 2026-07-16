//! Pipe-chain lowering: `|>` stages (single-argument action/child calls,
//! `.field` projections, and the deterministic combinator vocabulary),
//! lowered stage by stage into fresh bindings. The chain terminator (bind
//! or route) belongs to the caller.

use std::fmt::Write as _;

use crate::Span;
use crate::ast::{CombinatorKind, Expr, PipeStage, PipeStmt};

use super::collection_predicates::render_predicate_over;
use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, duration_expr, expr_type, field_type, render_expr};
use super::names::{ident, snake, string_lit};
use super::stmts::{CHILD_WITNESS, flush_prelude, retry_policy};
use super::types::{GType, type_ref_to_g};

/// The result type of one pipe stage applied to a value of `current` type.
pub(super) fn stage_type(
    emitter: &Emitter<'_>,
    current: &GType,
    stage: &PipeStage,
) -> Result<GType, EmitError> {
    match stage {
        PipeStage::Action { span, name } => {
            if let Some(&(_, action)) = emitter.actions.get(name.as_str()) {
                return Ok(type_ref_to_g(&action.returns));
            }
            if let Some(&child) = emitter.children.get(name.as_str()) {
                return Ok(type_ref_to_g(&child.returns));
            }
            Err(EmitError::new(
                *span,
                format!("`{name}` names neither a declared action nor a child workflow"),
            ))
        }
        PipeStage::Field { span, name } => field_type(emitter, current, name, *span),
        PipeStage::Combinator(combinator) => {
            let elem = || -> Result<GType, EmitError> {
                match emitter.env.resolve(current) {
                    GType::List(inner) => Ok(*inner),
                    other => Err(EmitError::new(
                        combinator.span,
                        format!(
                            "this combinator needs a list, found {}",
                            emitter.env.gleam_type(&other)
                        ),
                    )),
                }
            };
            match combinator.kind {
                CombinatorKind::Count => {
                    elem()?;
                    Ok(GType::Int)
                }
                CombinatorKind::Filter | CombinatorKind::Sort => {
                    elem()?;
                    Ok(current.clone())
                }
                CombinatorKind::Map => {
                    let elem = elem()?;
                    let Some(Expr::Accessor { span, name }) = combinator.arg.as_ref() else {
                        return Err(EmitError::new(
                            combinator.span,
                            "`map` takes a `.field` accessor in the Gleam stopgap",
                        ));
                    };
                    let projected = field_type(emitter, &elem, name, *span)?;
                    Ok(GType::List(Box::new(projected)))
                }
                CombinatorKind::Any | CombinatorKind::All => {
                    elem()?;
                    Ok(GType::Bool)
                }
            }
        }
    }
}

/// Render one combinator stage over the rendered `current` value.
fn render_combinator(
    emitter: &mut Emitter<'_>,
    current: &str,
    current_ty: &GType,
    combinator: &crate::ast::CombinatorCall,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    emitter.flags.uses_list_module = true;
    let accessor = || -> Result<String, EmitError> {
        match combinator.arg.as_ref() {
            Some(Expr::Accessor { name, .. }) => Ok(ident(name)),
            _ => Err(EmitError::new(
                combinator.span,
                "this combinator takes a `.field` accessor in the Gleam stopgap",
            )),
        }
    };
    match combinator.kind {
        CombinatorKind::Filter => {
            let field = accessor()?;
            Ok(format!(
                "list.filter({current}, fn(item) {{ item.{field} }})"
            ))
        }
        CombinatorKind::Map => {
            let field = accessor()?;
            Ok(format!("list.map({current}, fn(item) {{ item.{field} }})"))
        }
        CombinatorKind::Count => Ok(format!("list.length({current})")),
        CombinatorKind::Sort => {
            let field = accessor()?;
            let elem = match emitter.env.resolve(current_ty) {
                GType::List(inner) => *inner,
                _ => GType::Unknown,
            };
            let key_ty = match combinator.arg.as_ref() {
                Some(Expr::Accessor { span, name }) => field_type(emitter, &elem, name, *span)?,
                _ => GType::Unknown,
            };
            let compare = match emitter.env.resolve(&key_ty) {
                GType::Int => {
                    emitter.flags.compare_modules.insert("int");
                    "int.compare"
                }
                GType::Float => {
                    emitter.flags.compare_modules.insert("float");
                    "float.compare"
                }
                GType::Str => {
                    emitter.flags.compare_modules.insert("string");
                    "string.compare"
                }
                GType::Bool => {
                    emitter.flags.compare_modules.insert("bool");
                    "bool.compare"
                }
                other => {
                    return Err(EmitError::new(
                        combinator.span,
                        format!(
                            "`sort` needs a comparable key (Int, Float, String, Bool), found {}",
                            emitter.env.gleam_type(&other)
                        ),
                    ));
                }
            };
            Ok(format!(
                "list.sort({current}, fn(left, right) {{ {compare}(left.{field}, right.{field}) \
                 }})"
            ))
        }
        CombinatorKind::Any | CombinatorKind::All => {
            let element = match emitter.env.resolve(current_ty) {
                GType::List(inner) => *inner,
                other => {
                    return Err(EmitError::new(
                        combinator.span,
                        format!("collection predicate needs a list, found {other:?}"),
                    ));
                }
            };
            let predicate = combinator.arg.as_ref().ok_or_else(|| {
                EmitError::new(combinator.span, "collection predicate needs an argument")
            })?;
            let quantifier = if matches!(combinator.kind, CombinatorKind::Any) {
                crate::ast::Quantifier::Any
            } else {
                crate::ast::Quantifier::All
            };
            render_predicate_over(
                emitter, current, element, quantifier, predicate, scope, prelude,
            )
        }
    }
}

/// The value produced by a pipe chain, lowered stage by stage. Returns the
/// final rendered value and its type; the terminator is the caller's.
pub(super) fn lower_pipe_value(
    emitter: &mut Emitter<'_>,
    pipe: &PipeStmt,
    scope: &Scope,
) -> Result<(String, GType), EmitError> {
    let mut prelude = Vec::new();
    let mut current = render_expr(emitter, &pipe.head, scope, &mut prelude)?;
    let mut current_ty = expr_type(emitter, &pipe.head, scope)?;
    flush_prelude(emitter, prelude);
    for (position, stage) in pipe.stages.iter().enumerate() {
        let next_ty = stage_type(emitter, &current_ty, stage)?;
        let fresh = emitter.fresh_name(&format!("awl_piped_{position}"));
        match stage {
            PipeStage::Action { span, name } => {
                pipe_action_stage(emitter, *span, name, &fresh, &current, &current_ty)?;
            }
            PipeStage::Field { name, .. } => {
                emitter.line(&format!("let {fresh} = {current}.{}", ident(name)));
            }
            PipeStage::Combinator(combinator) => {
                let mut prelude = Vec::new();
                let rendered = render_combinator(
                    emitter,
                    &current,
                    &current_ty,
                    combinator,
                    scope,
                    &mut prelude,
                )?;
                flush_prelude(emitter, prelude);
                emitter.line(&format!("let {fresh} = {rendered}"));
            }
        }
        current = fresh;
        current_ty = next_ty;
    }
    Ok((current, current_ty))
}

/// One single-argument action or child stage of a pipe chain: the current
/// value threads in as the stage's one input.
fn pipe_action_stage(
    emitter: &mut Emitter<'_>,
    span: Span,
    name: &str,
    fresh: &str,
    current: &str,
    current_ty: &GType,
) -> Result<(), EmitError> {
    if let Some(&(queue, action)) = emitter.actions.get(name) {
        let [param] = action.params.as_slice() else {
            return Err(EmitError::new(
                span,
                format!(
                    "`{name}` takes {} arguments — a pipe stage needs exactly one",
                    action.params.len()
                ),
            ));
        };
        let param_ty = type_ref_to_g(&param.ty);
        let arg = wrap_optional(emitter, current.to_owned(), current_ty, &param_ty);
        // The wrapper call directly: config comes from the action
        // declaration alone in a pipe stage.
        let mut value = format!("{}_activity({arg})", snake(name));
        let config = action.config.as_ref();
        if let Some(retry) = config.and_then(|config| config.retry.as_ref()) {
            let _ = write!(value, " |> activity.retry({})", retry_policy(retry));
        }
        if let Some(timeout) = config.and_then(|config| config.timeout.as_ref()) {
            let _ = write!(value, " |> activity.timeout({})", duration_expr(timeout));
        }
        let _ = write!(value, " |> activity.task_queue({})", string_lit(queue));
        if let Some(node) = config.and_then(|config| config.node.as_ref()) {
            let _ = write!(value, " |> activity.node({})", string_lit(&node.name));
        }
        emitter.line(&format!(
            "use {fresh} <- result.try({value} |> workflow.run |> awl_error.map_activity_error)"
        ));
        return Ok(());
    }
    if let Some(&child) = emitter.children.get(name) {
        let [param] = child.params.as_slice() else {
            return Err(EmitError::new(
                span,
                format!(
                    "child `{name}` takes {} arguments — a pipe stage needs exactly one",
                    child.params.len()
                ),
            ));
        };
        let param_ty = type_ref_to_g(&param.ty);
        let to_json = emitter.to_json_fn(&param_ty);
        let arg = wrap_optional(emitter, current.to_owned(), current_ty, &param_ty);
        let input = format!(
            "json.object([#({}, {to_json}({arg}))])",
            string_lit(&param.name)
        );
        let output_codec = emitter.child_output_codec_fn(&type_ref_to_g(&child.returns));
        emitter.line(&format!(
            "use {fresh} <- result.try(workflow.spawn_and_wait({}, {CHILD_WITNESS}, {input}, \
             awlc.json_value(), {output_codec}(), awl_error.codec()) |> \
             awl_error.map_child_error)",
            string_lit(name)
        ));
        return Ok(());
    }
    Err(EmitError::new(
        span,
        format!("`{name}` names neither a declared action nor a child workflow"),
    ))
}

/// Wrap a rendered value in `Some(…)` when the slot is optional and the
/// value is not.
pub(super) fn wrap_optional(
    emitter: &Emitter<'_>,
    rendered: String,
    actual: &GType,
    expected: &GType,
) -> String {
    if matches!(emitter.env.resolve(expected), GType::Option(_))
        && !matches!(emitter.env.resolve(actual), GType::Option(_))
    {
        format!("Some({rendered})")
    } else {
        rendered
    }
}
