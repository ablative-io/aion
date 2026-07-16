//! Collapsed-region fan-out lowering — the MIR twin of the reference
//! `emitter/flows.rs::emit_fanout`. At a synthetic step whose body opens with
//! the `Distribute`/`Collect` marker pair, the per-item track dispatches over
//! the rendered collection:
//!
//! - a single-activity track fans out through the SDK combinators
//!   (`fanout_action`): `workflow.map` strict, `workflow.map_settled` with a
//!   per-slot `Option` substitution tolerant, `list.try_fold`/`list.fold`
//!   with `workflow.run` for `sequence`;
//! - a single declared-child track spawns all then awaits each in item order
//!   (`fanout_child`), or `spawn_and_wait`s per item for `sequence`;
//! - a multi-step `distribute` track runs each item as a synthesized child
//!   workflow (`fanout_child` over the implicit spawn tuple);
//! - a multi-step `sequence` track folds the generated instance wrapper one
//!   item at a time (strict `try_fold`, tolerant `fold` with `Option`
//!   capture).
//!
//! The gathered binding types as `List(item)` strict, `List(Option(item))`
//! tolerant — always one slot per input item, positionally aligned.

use std::collections::BTreeMap;

use crate::ast::{CallStmt, DeliveryVerb, DistributeStmt, Statement, Step};
use crate::emitter::{
    Emitter, GType, NestedPlan, RegionShape, implicit_child_required, single_member_call,
};
use crate::spanned::Spanned;

use super::super::func::FlowFn;
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{Block, LiveAfter, Stmt, Tail, Test, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::{call_rt, record_new};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_expr};
use super::flow::FlowEnv;
use super::slots::Slots;

/// The lifted-closure inventory one collapsed region step consumes from the
/// fork slot pool, mirroring the dispatch of [`lower_fanout`].
pub(super) fn count_fanout_fns(
    step: &Step,
    regions: &BTreeMap<String, RegionShape>,
    emitter: &Emitter<'_>,
) -> u32 {
    let Some(Statement::Distribute(_)) = step.body.first() else {
        return 0;
    };
    let Some(region) = regions.get(&step.name) else {
        return 0;
    };
    match single_member_call(region) {
        Some(call) if emitter.actions.contains_key(call.call.name.as_str()) => {
            // Tolerant distribute adds the per-slot substitution closure.
            if matches!(region.verb, DeliveryVerb::Distribute) && region.tolerant {
                2
            } else {
                1
            }
        }
        Some(call) if emitter.children.contains_key(call.call.name.as_str()) => match region.verb {
            DeliveryVerb::Distribute => 2,
            DeliveryVerb::Sequence => 1,
        },
        _ => match region.verb {
            DeliveryVerb::Distribute => 2,
            DeliveryVerb::Sequence => 1,
        },
    }
}

/// Whether this collapsed step's fan-out spawns children by string name at
/// the PARENT (the implicit multi-step-child form — declared-child tracks are
/// detected by the member-flow walk instead).
pub(super) fn needs_child_witness(
    step: &Step,
    regions: &BTreeMap<String, RegionShape>,
    emitter: &Emitter<'_>,
) -> bool {
    let Some(Statement::Distribute(_)) = step.body.first() else {
        return false;
    };
    let Some(region) = regions.get(&step.name) else {
        return false;
    };
    // The single declared-child track also spawns at the parent.
    if single_member_call(region)
        .is_some_and(|call| emitter.children.contains_key(call.call.name.as_str()))
    {
        return true;
    }
    implicit_child_required(emitter, region)
}

/// One collapsed region's resolved fan-out contract.
pub(super) struct Fanout<'a> {
    pub(super) region: &'a RegionShape,
    pub(super) nested: &'a NestedPlan,
    pub(super) item_ty: GType,
    pub(super) elem_ty: GType,
    pub(super) items: super::super::ops::Value,
    pub(super) span: crate::Span,
}

/// Lower the fan-out of one collapsed region step and bind the gathered
/// collection.
pub(super) fn lower_fanout(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    step: &Step,
    distribute: &DistributeStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let flow = env.flow;
    let region = flow.regions.get(&step.name).ok_or_else(|| {
        LowerError::new(
            distribute.span,
            format!("step `{}` lost its region shape", step.name),
        )
    })?;
    let nested = ctx.plans.regions.get(&region.id).ok_or_else(|| {
        LowerError::new(
            distribute.span,
            format!("step `{}` has no planned member flow", step.name),
        )
    })?;
    let item_ty = ctx
        .emitter
        .region_bindings
        .get(&region.id)
        .and_then(|bindings| bindings.get(&region.binding))
        .cloned()
        .ok_or_else(|| {
            LowerError::new(
                region.span,
                format!(
                    "the collected binding `{}` has no established type",
                    region.binding
                ),
            )
        })?;
    // R4: the collection expression evaluates BEFORE fan-out.
    let (items, items_ty) = lower_expr(ctx, &region.collection, scope, stmts)?;
    let elem_ty = match ctx.emitter.env.resolve(&items_ty) {
        GType::List(inner) => *inner,
        other => {
            return Err(LowerError::new(
                region.collection.span(),
                format!(
                    "`{}` needs a list, found {}",
                    region.verb.as_word(),
                    ctx.emitter.env.gleam_type(&other)
                ),
            ));
        }
    };
    let fanout = Fanout {
        region,
        nested,
        item_ty: item_ty.clone(),
        elem_ty,
        items,
        span: distribute.span,
    };

    let gathered = match single_member_call(region) {
        Some(call) if ctx.emitter.actions.contains_key(call.call.name.as_str()) => {
            super::fanout_action::lower_activity_fanout(
                ctx, env, &fanout, call, scope, stmts, slots,
            )?
        }
        Some(call) if ctx.emitter.children.contains_key(call.call.name.as_str()) => {
            super::fanout_child::lower_declared_child_fanout(
                ctx, env, &fanout, call, scope, stmts, slots,
            )?
        }
        _ => lower_instance_fanout(ctx, env, &fanout, scope, stmts, slots)?,
    };

    let slot = if region.tolerant {
        GType::Option(Box::new(item_ty))
    } else {
        item_ty
    };
    scope.insert(
        region.collect_bind.clone(),
        Binding {
            var: gathered,
            ty: GType::List(Box::new(slot)),
        },
    );
    Ok(())
}

/// Fan out a multi-step track: synthesized children for `distribute`, or the
/// generated instance wrapper one item at a time for `sequence`.
fn lower_instance_fanout(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    fanout: &Fanout<'_>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    if matches!(fanout.region.verb, DeliveryVerb::Distribute) {
        return super::fanout_child::lower_implicit_child_fanout(
            ctx, env, fanout, scope, stmts, slots,
        );
    }
    super::fanout_action::lower_instance_sequence(ctx, env, fanout, scope, stmts, slots)
}

/// The wrapper's free-name demand beyond the per-item variable, in wrapper
/// order (the per-item environment contract, `graph.rs::plan_flow`).
pub(super) fn wrapper_free_names(fanout: &Fanout<'_>) -> Vec<String> {
    fanout
        .nested
        .wrapper_params
        .iter()
        .filter(|name| **name != fanout.region.var)
        .cloned()
        .collect()
}

/// Whether a single-call track's arguments contain indexing (fallible
/// preludes cannot ride a parallel per-item track — the reference refusal).
pub(super) fn call_args_contain_index(call: &CallStmt) -> bool {
    call.call
        .args
        .iter()
        .any(|arg| super::expr::expr_contains_index(&arg.value))
}

/// The `List(Option(item))`/`List(item)` accumulator descriptor.
pub(super) fn gathered_desc(ctx: &Ctx<'_>, fanout: &Fanout<'_>) -> TyDesc {
    let item = ctx.tydesc(&fanout.item_ty);
    if fanout.region.tolerant {
        TyDesc::List(Box::new(TyDesc::Option(Box::new(item))))
    } else {
        TyDesc::List(Box::new(item))
    }
}

/// A lifted closure's frame: leading params, then one param per free name
/// (captures appended by `MakeClosure` at the call site, S9).
pub(super) fn closure_frame(
    ctx: &mut Ctx<'_>,
    span: crate::Span,
    host_scope: &Scope,
    free: &[String],
    leading: &[(Var, TyDesc)],
) -> Result<(Vec<Var>, Vec<TyDesc>, Scope), LowerError> {
    let mut params = Vec::new();
    let mut param_tys = Vec::new();
    for (var, ty) in leading {
        params.push(*var);
        param_tys.push(ty.clone());
    }
    let mut fn_scope = Scope::new();
    for name in free {
        let host = host_scope.get(name).ok_or_else(|| {
            LowerError::new(span, format!("fan-out free name `{name}` lost its binding"))
        })?;
        let var = ctx.fresh_var();
        params.push(var);
        param_tys.push(ctx.tydesc(&host.ty));
        fn_scope.insert(
            name.clone(),
            Binding {
                var,
                ty: host.ty.clone(),
            },
        );
    }
    Ok((params, param_tys, fn_scope))
}

/// The capture values (host scope bindings) a lifted closure closes over.
pub(super) fn capture_values(
    span: crate::Span,
    host_scope: &Scope,
    free: &[String],
) -> Result<Vec<super::super::ops::Value>, LowerError> {
    let mut captures = Vec::new();
    for name in free {
        let binding = host_scope.get(name).ok_or_else(|| {
            LowerError::new(span, format!("fan-out free name `{name}` lost its binding"))
        })?;
        captures.push(super::super::ops::Value::Var(binding.var));
    }
    Ok(captures)
}

/// Assemble one fan-out lifted function at its reserved fork-slot ordinal.
pub(super) fn fanout_fn(
    step_name: &str,
    span: crate::Span,
    ordinal: usize,
    frame: (Vec<Var>, Vec<TyDesc>),
    ret_ty: TyDesc,
    body: super::super::ops::Block,
) -> Result<super::super::func::FlowFn, LowerError> {
    let index = u32::try_from(ordinal).map_err(|_| LowerError::Planning {
        message: "fan-out ordinal exceeds u32".to_owned(),
    })?;
    let (params, param_tys) = frame;
    Ok(super::super::func::FlowFn {
        origin: super::super::func::FnOrigin::Fork {
            step: step_name.to_owned(),
            index,
        },
        name: format!("{}_fanout_{ordinal}", crate::emitter::snake(step_name)),
        params,
        param_tys,
        ret_ty,
        body,
        span: super::super::ids::Span::from_source(span),
        degraded_parallel: false,
    })
}

#[derive(Clone, Copy)]
pub(super) struct FoldResult {
    pub(super) result: Var,
    pub(super) acc: Var,
    pub(super) tolerant: bool,
    pub(super) map_error: Option<RuntimeFn>,
}

/// Close a fold body over its per-item result: strict try-capture + prepend
/// + `Ok(list)`, or the tolerant `Ok→Some / Error→None` slot capture.
pub(super) fn finish_fold_body(
    ctx: &mut Ctx<'_>,
    fanout: &Fanout<'_>,
    ordinal: usize,
    frame: (Vec<Var>, Vec<TyDesc>),
    acc_desc: TyDesc,
    mut stmts: Vec<Stmt>,
    fold: FoldResult,
) -> Result<FlowFn, LowerError> {
    let span = Span::from_source(fanout.span);
    if fold.tolerant {
        let ok = ctx.atom("ok");
        let some = ctx.atom("some");
        let none = ctx.atom("none");
        let payload = ctx.fresh_var();
        let wrapped = ctx.fresh_var();
        let kept = ctx.fresh_var();
        let then_block = Block {
            stmts: vec![
                Stmt::FieldGet {
                    dst: payload,
                    base: Value::Var(fold.result),
                    index: 1,
                    span,
                },
                Stmt::RecordNew {
                    dst: wrapped,
                    tag: some,
                    args: vec![Value::Var(payload)],
                    span,
                },
                Stmt::ListPrepend {
                    dst: kept,
                    head: Value::Var(wrapped),
                    tail: Value::Var(fold.acc),
                    span,
                },
            ],
            tail: Tail::Return(Value::Var(kept)),
        };
        let dropped = ctx.fresh_var();
        let else_block = Block {
            stmts: vec![Stmt::ListPrepend {
                dst: dropped,
                head: Value::Atom(none),
                tail: Value::Var(fold.acc),
                span,
            }],
            tail: Tail::Return(Value::Var(dropped)),
        };
        return fanout_fn(
            &fanout.region.open_name,
            fanout.span,
            ordinal,
            frame,
            acc_desc,
            Block {
                stmts,
                tail: Tail::If {
                    test: Test::IsTagged {
                        value: Value::Var(fold.result),
                        tag: ok,
                        arity: 2,
                    },
                    then_block: Box::new(then_block),
                    else_block: Box::new(else_block),
                    span,
                },
            },
        );
    }
    let mapped = match fold.map_error {
        Some(mapper) => call_rt(
            ctx,
            mapper,
            vec![Value::Var(fold.result)],
            &mut stmts,
            fanout.span,
        ),
        None => fold.result,
    };
    let bound = try_bind(ctx, mapped, &mut stmts, fanout.span);
    let consed = ctx.fresh_var();
    stmts.push(Stmt::ListPrepend {
        dst: consed,
        head: Value::Var(bound),
        tail: Value::Var(fold.acc),
        span,
    });
    let ok = ctx.atom("ok");
    let ok_var = record_new(ctx, ok, vec![Value::Var(consed)], &mut stmts);
    fanout_fn(
        &fanout.region.open_name,
        fanout.span,
        ordinal,
        frame,
        TyDesc::Result(Box::new(acc_desc), Box::new(TyDesc::AwlError)),
        Block {
            stmts,
            tail: Tail::Return(Value::Var(ok_var)),
        },
    )
}

/// The sequential fold's call site: `list.try_fold` + `TryBind` (strict) or
/// `list.fold` (tolerant), reversed once.
pub(super) fn fold_call_site(
    ctx: &mut Ctx<'_>,
    fanout: &Fanout<'_>,
    self_ref: FnRef,
    free: &[String],
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    tolerant: bool,
) -> Result<Var, LowerError> {
    let captures = capture_values(fanout.span, scope, free)?;
    let closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: closure,
        lifted: self_ref,
        captures,
        span: Span::from_source(fanout.span),
    });
    let folded = if tolerant {
        call_rt(
            ctx,
            RuntimeFn::LFold,
            vec![fanout.items.clone(), Value::Nil, Value::Var(closure)],
            stmts,
            fanout.span,
        )
    } else {
        let result = call_rt(
            ctx,
            RuntimeFn::LTryFold,
            vec![fanout.items.clone(), Value::Nil, Value::Var(closure)],
            stmts,
            fanout.span,
        );
        try_bind(ctx, result, stmts, fanout.span)
    };
    Ok(call_rt(
        ctx,
        RuntimeFn::LReverse,
        vec![Value::Var(folded)],
        stmts,
        fanout.span,
    ))
}

pub(super) fn try_bind(
    ctx: &mut Ctx<'_>,
    result: Var,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Var {
    let dst = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst,
        result,
        live_after: LiveAfter::default(),
        span: Span::from_source(span),
    });
    dst
}
