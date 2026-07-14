//! Pipe-chain lowering on the direct path — the MIR twin of the reference
//! `emitter/pipes.rs`: `|>` stages (single-argument action/child calls,
//! `.field` projections, and the deterministic combinator vocabulary) lowered
//! stage by stage into fresh SSA values. Combinator projection/compare
//! closures ride the dynamically-allocated lifted-closure slots
//! (`Ctx::take_predicate`), exactly like collection predicates; post-stage
//! `any`/`all` delegate to `lower_predicate_over`. The chain terminator (bind
//! or route) belongs to the caller (`flow`).

use crate::ast::{Call, CombinatorCall, CombinatorKind, Expr, PipeStage, Quantifier};
use crate::emitter::{GType, type_ref_to_g};

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{Block, Stmt, Tail, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::{activity_call, call_rt};
use super::build::FnPlan;
use super::child_call::pipe_child_stage;
use super::collection_predicate::{PredicateOver, lower_predicate_over};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Scope, lower_expr};

/// The value produced by a pipe chain, lowered stage by stage. Returns the
/// final value and its type; the terminator is the caller's.
pub(super) fn lower_pipe_value(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    head: &Expr,
    stages: &[PipeStage],
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(Value, GType), LowerError> {
    let (mut value, mut ty) = lower_expr(ctx, head, scope, stmts)?;
    for stage in stages {
        match stage {
            PipeStage::Action { name, span } => {
                if ctx.emitter.actions.contains_key(name.as_str()) {
                    let call = Call {
                        span: *span,
                        name: name.clone(),
                        name_span: *span,
                        args: Vec::new(),
                    };
                    let bound = activity_call(ctx, plan, &call, Some((value, ty)), scope, stmts)?;
                    ty = ctx
                        .emitter
                        .actions
                        .get(name.as_str())
                        .map_or(GType::Unknown, |&(_, decl)| type_ref_to_g(&decl.returns));
                    value = Value::Var(bound);
                } else if ctx.emitter.children.contains_key(name.as_str()) {
                    let (bound, returns) =
                        pipe_child_stage(ctx, plan, name, *span, (value, ty), stmts)?;
                    value = Value::Var(bound);
                    ty = returns;
                } else {
                    // The reference's exact diagnostic (`emitter/pipes.rs`).
                    return Err(LowerError::new(
                        *span,
                        format!("`{name}` names neither a declared action nor a child workflow"),
                    ));
                }
            }
            PipeStage::Field { name, span } => {
                let (index, field_ty) = field_index(ctx, &ty, name, *span)?;
                let dst = ctx.fresh_var();
                stmts.push(Stmt::FieldGet {
                    dst,
                    base: value,
                    index,
                    span: Span::from_source(*span),
                });
                value = Value::Var(dst);
                ty = field_ty;
            }
            PipeStage::Combinator(combinator) => {
                let (next_value, next_ty) =
                    lower_combinator_stage(ctx, value, &ty, combinator, scope, stmts)?;
                value = next_value;
                ty = next_ty;
            }
        }
    }
    Ok((value, ty))
}

/// One combinator stage over the lowered `current` value, mirroring the
/// reference `render_combinator` + `stage_type` pair.
fn lower_combinator_stage(
    ctx: &mut Ctx<'_>,
    current: Value,
    current_ty: &GType,
    combinator: &CombinatorCall,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(Value, GType), LowerError> {
    // Checker-unreachable: combinators only check over list-typed stages.
    let GType::List(elem) = ctx.emitter.env.resolve(current_ty) else {
        return Err(LowerError::new(
            combinator.span,
            "this combinator needs a list",
        ));
    };
    match combinator.kind {
        CombinatorKind::Count => {
            let counted = call_rt(
                ctx,
                RuntimeFn::LLength,
                vec![current],
                stmts,
                combinator.span,
            );
            Ok((Value::Var(counted), GType::Int))
        }
        CombinatorKind::Filter => {
            let (field, field_span) = accessor_arg(combinator)?;
            let (closure, _) = projection_closure(ctx, &elem, field, field_span, stmts)?;
            let filtered = call_rt(
                ctx,
                RuntimeFn::LFilter,
                vec![current, Value::Var(closure)],
                stmts,
                combinator.span,
            );
            Ok((Value::Var(filtered), current_ty.clone()))
        }
        CombinatorKind::Map => {
            let (field, field_span) = accessor_arg(combinator)?;
            let (closure, field_ty) = projection_closure(ctx, &elem, field, field_span, stmts)?;
            let mapped = call_rt(
                ctx,
                RuntimeFn::LMap,
                vec![current, Value::Var(closure)],
                stmts,
                combinator.span,
            );
            Ok((Value::Var(mapped), GType::List(Box::new(field_ty))))
        }
        CombinatorKind::Sort => {
            let (field, field_span) = accessor_arg(combinator)?;
            let (_, key_ty) = field_index(ctx, &elem, field, field_span)?;
            let compare = match ctx.emitter.env.resolve(&key_ty) {
                GType::Int => RuntimeFn::CmpInt,
                GType::Float => RuntimeFn::CmpFloat,
                GType::Str => RuntimeFn::CmpString,
                GType::Bool => RuntimeFn::CmpBool,
                // The reference's hard gate (`emitter/pipes.rs`), mapped to
                // the direct path's refusal class.
                _ => {
                    return Err(LowerError::unsupported(
                        "`sort` over a non-comparable key (needs Int, Float, String, Bool)",
                        combinator.span,
                    ));
                }
            };
            let closure = compare_closure(ctx, &elem, field, compare, field_span, stmts)?;
            let sorted = call_rt(
                ctx,
                RuntimeFn::LSort,
                vec![current, Value::Var(closure)],
                stmts,
                combinator.span,
            );
            Ok((Value::Var(sorted), current_ty.clone()))
        }
        CombinatorKind::Any | CombinatorKind::All => {
            let predicate = combinator.arg.as_ref().ok_or_else(|| {
                LowerError::new(combinator.span, "collection predicate needs an argument")
            })?;
            let quantifier = if matches!(combinator.kind, CombinatorKind::Any) {
                Quantifier::Any
            } else {
                Quantifier::All
            };
            lower_predicate_over(
                ctx,
                PredicateOver {
                    items: current,
                    element: &elem,
                    quantifier,
                    predicate,
                    span: combinator.span,
                },
                scope,
                stmts,
            )
        }
    }
}

/// The `.field` accessor a filter/map/sort combinator takes.
fn accessor_arg(combinator: &CombinatorCall) -> Result<(&str, crate::Span), LowerError> {
    match combinator.arg.as_ref() {
        Some(Expr::Accessor { name, span }) => Ok((name, *span)),
        _ => Err(LowerError::new(
            combinator.span,
            "this combinator takes a `.field` accessor in the Gleam stopgap",
        )),
    }
}

/// A capture-free lifted `fn(item) { item.field }` twin, plus its host-side
/// closure value. Returns the closure var and the projected field type.
fn projection_closure(
    ctx: &mut Ctx<'_>,
    elem: &GType,
    field: &str,
    span: crate::Span,
    stmts: &mut Vec<Stmt>,
) -> Result<(Var, GType), LowerError> {
    let (index, field_ty) = field_index(ctx, elem, field, span)?;
    let (ordinal, reference) = ctx.take_predicate()?;
    let saved = ctx.swap_var_counter(0);
    let built = build_projection_fn(ctx, ordinal, elem, index, &field_ty, span);
    ctx.swap_var_counter(saved);
    ctx.finish_predicate(ordinal, MirFn::Flow(built));
    let closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: closure,
        lifted: reference,
        captures: Vec::new(),
        span: Span::from_source(span),
    });
    Ok((closure, field_ty))
}

fn build_projection_fn(
    ctx: &mut Ctx<'_>,
    ordinal: usize,
    elem: &GType,
    index: u16,
    field_ty: &GType,
    span: crate::Span,
) -> FlowFn {
    let item = ctx.fresh_var();
    let dst = ctx.fresh_var();
    let body_stmts = vec![Stmt::FieldGet {
        dst,
        base: Value::Var(item),
        index,
        span: Span::from_source(span),
    }];
    FlowFn {
        origin: combinator_origin(ordinal),
        name: format!("awl_combinator_{ordinal}"),
        params: vec![item],
        param_tys: vec![ctx.tydesc(elem)],
        ret_ty: ctx.tydesc(field_ty),
        body: Block {
            stmts: body_stmts,
            tail: Tail::Return(Value::Var(dst)),
        },
        span: Span::from_source(span),
        degraded_parallel: false,
    }
}

/// A capture-free lifted `fn(left, right) { cmp(left.field, right.field) }`
/// twin for `sort`, plus its host-side closure value.
fn compare_closure(
    ctx: &mut Ctx<'_>,
    elem: &GType,
    field: &str,
    compare: RuntimeFn,
    span: crate::Span,
    stmts: &mut Vec<Stmt>,
) -> Result<Var, LowerError> {
    let (index, _) = field_index(ctx, elem, field, span)?;
    let (ordinal, reference) = ctx.take_predicate()?;
    let saved = ctx.swap_var_counter(0);
    let built = build_compare_fn(ctx, ordinal, elem, index, compare, span);
    ctx.swap_var_counter(saved);
    ctx.finish_predicate(ordinal, MirFn::Flow(built));
    let closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: closure,
        lifted: reference,
        captures: Vec::new(),
        span: Span::from_source(span),
    });
    Ok(closure)
}

fn build_compare_fn(
    ctx: &mut Ctx<'_>,
    ordinal: usize,
    elem: &GType,
    index: u16,
    compare: RuntimeFn,
    span: crate::Span,
) -> FlowFn {
    let left = ctx.fresh_var();
    let right = ctx.fresh_var();
    let left_key = ctx.fresh_var();
    let right_key = ctx.fresh_var();
    let body_stmts = vec![
        Stmt::FieldGet {
            dst: left_key,
            base: Value::Var(left),
            index,
            span: Span::from_source(span),
        },
        Stmt::FieldGet {
            dst: right_key,
            base: Value::Var(right),
            index,
            span: Span::from_source(span),
        },
    ];
    let elem_desc = ctx.tydesc(elem);
    FlowFn {
        origin: combinator_origin(ordinal),
        name: format!("awl_combinator_{ordinal}"),
        params: vec![left, right],
        param_tys: vec![elem_desc.clone(), elem_desc],
        ret_ty: TyDesc::Custom {
            module: "gleam/order".to_owned(),
            name: "Order".to_owned(),
            params: Vec::new(),
        },
        body: Block {
            stmts: body_stmts,
            tail: Tail::TailRt {
                callee: compare,
                args: vec![Value::Var(left_key), Value::Var(right_key)],
            },
        },
        span: Span::from_source(span),
        degraded_parallel: false,
    }
}

fn combinator_origin(ordinal: usize) -> FnOrigin {
    FnOrigin::LiftedClosure {
        host: FnRef(2),
        index: u32::try_from(ordinal).map_or(u32::MAX, |index| index),
    }
}

fn field_index(
    ctx: &Ctx<'_>,
    base_ty: &GType,
    field: &str,
    span: crate::Span,
) -> Result<(u16, GType), LowerError> {
    let (_, record) = ctx
        .emitter
        .env
        .record_of(base_ty)
        .ok_or_else(|| LowerError::new(span, format!("`.{field}` needs a record")))?;
    let position = record
        .fields
        .iter()
        .position(|candidate| candidate.awl_name == field)
        .ok_or_else(|| LowerError::new(span, format!("no field `{field}`")))?;
    Ok((
        u16::try_from(position + 1).unwrap_or(u16::MAX),
        record.fields[position].ty.clone(),
    ))
}
