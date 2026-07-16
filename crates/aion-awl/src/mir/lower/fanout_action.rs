//! Single-activity and instance-sequence fan-out tracks (the MIR twin of
//! `emitter/flows.rs::emit_activity_fanout`/`emit_instance_fanout`):
//!
//! - `distribute` strict: one lifted map body returning the unrun configured
//!   activity value, dispatched through `workflow.map |>
//!   awl_error.map_activity_error`;
//! - `distribute` tolerant: the same map body through
//!   `workflow.map_settled`, then a per-slot `Ok→Some / Error→None`
//!   substitution via `list.map`;
//! - `sequence` strict/tolerant: `list.try_fold`/`list.fold` running one
//!   activity per item (`workflow.run`), prepending, reversed once;
//! - multi-step `sequence`: the same folds calling the region's generated
//!   instance wrapper one item at a time.

use crate::ast::{CallStmt, DeliveryVerb};

use super::super::func::{FlowFn, MirFn};
use super::super::ids::{Span, Var};
use super::super::ops::{Block, LiveAfter, Stmt, Tail, Test, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::{ActivityForm, activity_value, call_rt};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope};
use super::fanout::{
    Fanout, FoldResult, call_args_contain_index, capture_values, closure_frame, fanout_fn,
    finish_fold_body, fold_call_site, gathered_desc, try_bind, wrapper_free_names,
};
use super::flow::FlowEnv;
use super::forks::branch_free_names;
use super::slots::Slots;

/// One single-activity track's lowering surface.
struct Track<'a> {
    env: FlowEnv<'a>,
    fanout: &'a Fanout<'a>,
    call: &'a CallStmt,
    free: &'a [String],
    scope: &'a Scope,
}

/// Fan out a single-activity track through the SDK combinators.
pub(super) fn lower_activity_fanout(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    fanout: &Fanout<'_>,
    call: &CallStmt,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let free = branch_free_names(&call.call, &fanout.region.var, scope);
    let track = Track {
        env,
        fanout,
        call,
        free: &free,
        scope,
    };
    match (fanout.region.verb, fanout.region.tolerant) {
        (DeliveryVerb::Distribute, tolerant) => {
            if call_args_contain_index(call) {
                return Err(LowerError::unsupported(
                    "indexing inside a parallel per-item track",
                    call.span,
                ));
            }
            let map_closure = build_map_closure(ctx, &track, stmts, slots)?;
            if tolerant {
                let settled = call_rt(
                    ctx,
                    RuntimeFn::WfMapSettled,
                    vec![fanout.items.clone(), Value::Var(map_closure)],
                    stmts,
                    fanout.span,
                );
                let mapper = build_slot_mapper(ctx, fanout, stmts, slots)?;
                Ok(call_rt(
                    ctx,
                    RuntimeFn::LMap,
                    vec![Value::Var(settled), Value::Var(mapper)],
                    stmts,
                    fanout.span,
                ))
            } else {
                let ran = call_rt(
                    ctx,
                    RuntimeFn::WfMap,
                    vec![fanout.items.clone(), Value::Var(map_closure)],
                    stmts,
                    fanout.span,
                );
                let mapped = call_rt(
                    ctx,
                    RuntimeFn::MapActivityError,
                    vec![Value::Var(ran)],
                    stmts,
                    fanout.span,
                );
                Ok(try_bind(ctx, mapped, stmts, fanout.span))
            }
        }
        (DeliveryVerb::Sequence, tolerant) => {
            let (ordinal, self_ref) = slots.forks.take()?;
            let saved = ctx.swap_var_counter(0);
            let body = build_activity_fold(ctx, &track, ordinal, tolerant);
            ctx.swap_var_counter(saved);
            slots.forks.finish(ordinal, MirFn::Flow(body?));
            fold_call_site(ctx, fanout, self_ref, &free, scope, stmts, tolerant)
        }
    }
}

/// Fan out a multi-step `sequence` track: the generated instance wrapper one
/// item at a time.
pub(super) fn lower_instance_sequence(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    fanout: &Fanout<'_>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let free = wrapper_free_names(fanout);
    let tolerant = fanout.region.tolerant;
    let (ordinal, self_ref) = slots.forks.take()?;
    let saved = ctx.swap_var_counter(0);
    let body = build_instance_fold(ctx, env, fanout, &free, scope, ordinal, tolerant);
    ctx.swap_var_counter(saved);
    slots.forks.finish(ordinal, MirFn::Flow(body?));
    fold_call_site(ctx, fanout, self_ref, &free, scope, stmts, tolerant)
}

/// The parallel map body: returns the unrun configured activity value for
/// `workflow.map`/`workflow.map_settled` to dispatch.
fn build_map_closure(
    ctx: &mut Ctx<'_>,
    track: &Track<'_>,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let fanout = track.fanout;
    let (ordinal, self_ref) = slots.forks.take()?;
    let saved = ctx.swap_var_counter(0);
    let built = build_map_body(ctx, track, ordinal);
    ctx.swap_var_counter(saved);
    slots.forks.finish(ordinal, MirFn::Flow(built?));
    let captures = capture_values(fanout.span, track.scope, track.free)?;
    let closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: closure,
        lifted: self_ref,
        captures,
        span: Span::from_source(fanout.span),
    });
    Ok(closure)
}

fn build_map_body(
    ctx: &mut Ctx<'_>,
    track: &Track<'_>,
    ordinal: usize,
) -> Result<FlowFn, LowerError> {
    let fanout = track.fanout;
    let item = ctx.fresh_var();
    let elem_desc = ctx.tydesc(&fanout.elem_ty);
    let (params, param_tys, mut fn_scope) = closure_frame(
        ctx,
        fanout.span,
        track.scope,
        track.free,
        &[(item, elem_desc)],
    )?;
    fn_scope.insert(
        fanout.region.var.clone(),
        Binding {
            var: item,
            ty: fanout.elem_ty.clone(),
        },
    );
    let mut body_stmts = Vec::new();
    let queued = activity_value(
        ctx,
        track.env.plan,
        &track.call.call,
        ActivityForm {
            site_config: track.call.config.as_ref(),
            piped: None,
            raw: false,
        },
        &fn_scope,
        &mut body_stmts,
    )?;
    let input_name = ctx.emitter.action_inputs[track.call.call.name.as_str()].clone();
    let ret_ty = TyDesc::Activity(
        Box::new(TyDesc::Custom {
            module: ctx.module_name.clone(),
            name: input_name,
            params: Vec::new(),
        }),
        Box::new(ctx.tydesc(&fanout.item_ty)),
    );
    fanout_fn(
        &fanout.region.open_name,
        fanout.span,
        ordinal,
        (params, param_tys),
        ret_ty,
        Block {
            stmts: body_stmts,
            tail: Tail::Return(Value::Var(queued)),
        },
    )
}

/// The tolerant per-slot substitution closure: `Ok(item) -> Some(item),
/// Error(_) -> None`.
fn build_slot_mapper(
    ctx: &mut Ctx<'_>,
    fanout: &Fanout<'_>,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let (ordinal, self_ref) = slots.forks.take()?;
    let saved = ctx.swap_var_counter(0);
    let built = build_slot_mapper_body(ctx, fanout, ordinal);
    ctx.swap_var_counter(saved);
    slots.forks.finish(ordinal, MirFn::Flow(built?));
    let mapper = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: mapper,
        lifted: self_ref,
        captures: Vec::new(),
        span: Span::from_source(fanout.span),
    });
    Ok(mapper)
}

fn build_slot_mapper_body(
    ctx: &mut Ctx<'_>,
    fanout: &Fanout<'_>,
    ordinal: usize,
) -> Result<FlowFn, LowerError> {
    let slot = ctx.fresh_var();
    let item_desc = ctx.tydesc(&fanout.item_ty);
    let slot_desc = TyDesc::Result(Box::new(item_desc.clone()), Box::new(TyDesc::AwlError));
    let ok = ctx.atom("ok");
    let some = ctx.atom("some");
    let none = ctx.atom("none");
    let span = Span::from_source(fanout.span);
    let payload = ctx.fresh_var();
    let wrapped = ctx.fresh_var();
    let then_block = Block {
        stmts: vec![
            Stmt::FieldGet {
                dst: payload,
                base: Value::Var(slot),
                index: 1,
                span,
            },
            Stmt::RecordNew {
                dst: wrapped,
                tag: some,
                args: vec![Value::Var(payload)],
                span,
            },
        ],
        tail: Tail::Return(Value::Var(wrapped)),
    };
    let else_block = Block {
        stmts: Vec::new(),
        tail: Tail::Return(Value::Atom(none)),
    };
    fanout_fn(
        &fanout.region.open_name,
        fanout.span,
        ordinal,
        (vec![slot], vec![slot_desc]),
        TyDesc::Option(Box::new(item_desc)),
        Block {
            stmts: Vec::new(),
            tail: Tail::If {
                test: Test::IsTagged {
                    value: Value::Var(slot),
                    tag: ok,
                    arity: 2,
                },
                then_block: Box::new(then_block),
                else_block: Box::new(else_block),
                span,
            },
        },
    )
}

/// A sequential single-activity fold body: run one activity, capture (strict
/// try / tolerant `Option`), prepend.
fn build_activity_fold(
    ctx: &mut Ctx<'_>,
    track: &Track<'_>,
    ordinal: usize,
    tolerant: bool,
) -> Result<FlowFn, LowerError> {
    let fanout = track.fanout;
    let acc = ctx.fresh_var();
    let item = ctx.fresh_var();
    let acc_desc = gathered_desc(ctx, fanout);
    let elem_desc = ctx.tydesc(&fanout.elem_ty);
    let (params, param_tys, mut fn_scope) = closure_frame(
        ctx,
        fanout.span,
        track.scope,
        track.free,
        &[(acc, acc_desc.clone()), (item, elem_desc)],
    )?;
    fn_scope.insert(
        fanout.region.var.clone(),
        Binding {
            var: item,
            ty: fanout.elem_ty.clone(),
        },
    );
    let mut stmts = Vec::new();
    let queued = activity_value(
        ctx,
        track.env.plan,
        &track.call.call,
        ActivityForm {
            site_config: track.call.config.as_ref(),
            piped: None,
            raw: false,
        },
        &fn_scope,
        &mut stmts,
    )?;
    let ran = call_rt(
        ctx,
        RuntimeFn::WfRun,
        vec![Value::Var(queued)],
        &mut stmts,
        track.call.call.name_span,
    );
    finish_fold_body(
        ctx,
        fanout,
        ordinal,
        (params, param_tys),
        acc_desc,
        stmts,
        FoldResult {
            result: ran,
            acc,
            tolerant,
            map_error: Some(RuntimeFn::MapActivityError),
        },
    )
}

/// A multi-step `sequence` fold body: one instance wrapper call per item.
fn build_instance_fold(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    fanout: &Fanout<'_>,
    free: &[String],
    scope: &Scope,
    ordinal: usize,
    tolerant: bool,
) -> Result<FlowFn, LowerError> {
    let acc = ctx.fresh_var();
    let item = ctx.fresh_var();
    let acc_desc = gathered_desc(ctx, fanout);
    let elem_desc = ctx.tydesc(&fanout.elem_ty);
    let (params, param_tys, mut fn_scope) = closure_frame(
        ctx,
        fanout.span,
        scope,
        free,
        &[(acc, acc_desc.clone()), (item, elem_desc)],
    )?;
    fn_scope.insert(
        fanout.region.var.clone(),
        Binding {
            var: item,
            ty: fanout.elem_ty.clone(),
        },
    );
    let wrapper = env
        .plan
        .region_fns
        .get(&fanout.region.id)
        .map(|fns| fns.wrapper)
        .ok_or_else(|| LowerError::Planning {
            message: format!("region {} was never planned", fanout.region.id),
        })?;
    let mut args = Vec::new();
    for name in &fanout.nested.wrapper_params {
        let binding = fn_scope.get(name).ok_or_else(|| {
            LowerError::new(
                fanout.span,
                format!("instance argument `{name}` lost its binding"),
            )
        })?;
        args.push(Value::Var(binding.var));
    }
    let mut stmts = Vec::new();
    let ran = ctx.fresh_var();
    stmts.push(Stmt::CallLocal {
        dst: Some(ran),
        callee: wrapper,
        args,
        live_after: LiveAfter::default(),
        span: Span::from_source(fanout.span),
    });
    finish_fold_body(
        ctx,
        fanout,
        ordinal,
        (params, param_tys),
        acc_desc,
        stmts,
        FoldResult {
            result: ran,
            acc,
            tolerant,
            map_error: None,
        },
    )
}
