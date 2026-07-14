//! Action collection-fork lowering: lifted `workflow.map`/`try_fold` bodies
//! plus their host call sites. Kept separate from branch classification and
//! child fan-out so each lowering unit stays within the workspace line limit.

use crate::ast::{Call, ForkStmt, Step};
use crate::emitter::{GType, snake};

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{Span, Var};
use super::super::ops::{LiveAfter, Stmt, Tail, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::{activity_value, call_rt, record_new};
use super::build::FnPlan;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope};
use super::slots::Slots;

pub(super) struct ActionFork<'a> {
    pub(super) plan: &'a FnPlan,
    pub(super) step: &'a Step,
    pub(super) fork: &'a ForkStmt,
    pub(super) call: &'a Call,
    pub(super) var: &'a str,
    pub(super) items: Value,
    pub(super) elem_ty: &'a GType,
    pub(super) returns: &'a GType,
    pub(super) free: &'a [String],
    pub(super) scope: &'a Scope,
    pub(super) sequential: bool,
}

pub(super) fn lower_action_collection(
    ctx: &mut Ctx<'_>,
    fork: ActionFork<'_>,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let (ordinal, self_ref) = slots.forks.take()?;
    let build = ForkBuild {
        plan: fork.plan,
        step: fork.step,
        call: fork.call,
        var: fork.var,
        elem_ty: fork.elem_ty,
        returns: fork.returns,
        free: fork.free,
        host_scope: fork.scope,
        ordinal,
        span: fork.fork.span,
    };
    let saved = ctx.swap_var_counter(0);
    let function = if fork.sequential {
        build_fold_body(ctx, &build)
    } else {
        build_map_body(ctx, &build)
    };
    ctx.swap_var_counter(saved);
    slots.forks.finish(ordinal, MirFn::Flow(function?));

    let span = Span::from_source(fork.fork.span);
    let mut captures = Vec::new();
    for name in fork.free {
        let binding = fork.scope.get(name).ok_or_else(|| {
            LowerError::new(
                fork.fork.span,
                format!("fork free name `{name}` lost its binding"),
            )
        })?;
        captures.push(Value::Var(binding.var));
    }
    let closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: closure,
        lifted: self_ref,
        captures,
        span,
    });
    Ok(fan_out(
        ctx,
        fork.fork,
        fork.sequential,
        fork.items,
        closure,
        stmts,
    ))
}

/// The fan-out call site: `workflow.map |> map_activity_error` (parallel) or
/// `list.try_fold(items, [], …)` + `list.reverse` (sequential).
fn fan_out(
    ctx: &mut Ctx<'_>,
    fork: &ForkStmt,
    sequential: bool,
    items_value: Value,
    closure: Var,
    stmts: &mut Vec<Stmt>,
) -> Var {
    let span = Span::from_source(fork.span);
    if sequential {
        let folded_result = call_rt(
            ctx,
            RuntimeFn::LTryFold,
            vec![items_value, Value::Nil, Value::Var(closure)],
            stmts,
            fork.span,
        );
        let folded = ctx.fresh_var();
        stmts.push(Stmt::TryBind {
            dst: folded,
            result: folded_result,
            live_after: LiveAfter::default(),
            span,
        });
        call_rt(
            ctx,
            RuntimeFn::LReverse,
            vec![Value::Var(folded)],
            stmts,
            fork.span,
        )
    } else {
        let ran = call_rt(
            ctx,
            RuntimeFn::WfMap,
            vec![items_value, Value::Var(closure)],
            stmts,
            fork.span,
        );
        let mapped = call_rt(
            ctx,
            RuntimeFn::MapActivityError,
            vec![Value::Var(ran)],
            stmts,
            fork.span,
        );
        let bound = ctx.fresh_var();
        stmts.push(Stmt::TryBind {
            dst: bound,
            result: mapped,
            live_after: LiveAfter::default(),
            span,
        });
        bound
    }
}

struct ForkBuild<'a> {
    plan: &'a FnPlan,
    step: &'a Step,
    call: &'a Call,
    var: &'a str,
    elem_ty: &'a GType,
    returns: &'a GType,
    free: &'a [String],
    host_scope: &'a Scope,
    ordinal: usize,
    span: crate::Span,
}

fn closure_frame(
    ctx: &mut Ctx<'_>,
    build: &ForkBuild<'_>,
    leading: &[(Var, TyDesc)],
) -> Result<(Vec<Var>, Vec<TyDesc>, Scope), LowerError> {
    let mut params = Vec::new();
    let mut param_tys = Vec::new();
    for (var, ty) in leading {
        params.push(*var);
        param_tys.push(ty.clone());
    }
    let mut fn_scope = Scope::new();
    for name in build.free {
        let host = build.host_scope.get(name).ok_or_else(|| {
            LowerError::new(
                build.span,
                format!("fork free name `{name}` lost its binding"),
            )
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

fn fork_fn(
    build: &ForkBuild<'_>,
    frame: (Vec<Var>, Vec<TyDesc>),
    ret_ty: TyDesc,
    body: super::super::ops::Block,
) -> Result<FlowFn, LowerError> {
    let index = u32::try_from(build.ordinal).map_err(|_| LowerError::Planning {
        message: "fork ordinal exceeds u32".to_owned(),
    })?;
    let (params, param_tys) = frame;
    Ok(FlowFn {
        origin: FnOrigin::Fork {
            step: build.step.name.clone(),
            index,
        },
        name: format!("{}_fork_{}", snake(&build.step.name), build.ordinal),
        params,
        param_tys,
        ret_ty,
        body,
        span: Span::from_source(build.span),
        degraded_parallel: false,
    })
}

/// The parallel body returns the unrun configured activity value for
/// `workflow.map` to dispatch.
fn build_map_body(ctx: &mut Ctx<'_>, build: &ForkBuild<'_>) -> Result<FlowFn, LowerError> {
    let item = ctx.fresh_var();
    let elem_desc = ctx.tydesc(build.elem_ty);
    let (params, param_tys, mut fn_scope) = closure_frame(ctx, build, &[(item, elem_desc)])?;
    fn_scope.insert(
        build.var.to_owned(),
        Binding {
            var: item,
            ty: build.elem_ty.clone(),
        },
    );
    let mut stmts = Vec::new();
    let queued = activity_value(
        ctx, build.plan, build.call, None, &fn_scope, &mut stmts, false,
    )?;
    let input_name = ctx.emitter.action_inputs[build.call.name.as_str()].clone();
    let ret_ty = TyDesc::Activity(
        Box::new(TyDesc::Custom {
            module: ctx.module_name.clone(),
            name: input_name,
            params: Vec::new(),
        }),
        Box::new(ctx.tydesc(build.returns)),
    );
    fork_fn(
        build,
        (params, param_tys),
        ret_ty,
        super::super::ops::Block {
            stmts,
            tail: Tail::Return(Value::Var(queued)),
        },
    )
}

/// The sequential body runs one activity and prepends its result to the fold
/// accumulator. The host reverses the accumulator once after the fold.
fn build_fold_body(ctx: &mut Ctx<'_>, build: &ForkBuild<'_>) -> Result<FlowFn, LowerError> {
    let acc = ctx.fresh_var();
    let item = ctx.fresh_var();
    let elem_desc = ctx.tydesc(build.elem_ty);
    let acc_desc = TyDesc::List(Box::new(ctx.tydesc(build.returns)));
    let (params, param_tys, mut fn_scope) =
        closure_frame(ctx, build, &[(acc, acc_desc.clone()), (item, elem_desc)])?;
    fn_scope.insert(
        build.var.to_owned(),
        Binding {
            var: item,
            ty: build.elem_ty.clone(),
        },
    );
    let mut stmts = Vec::new();
    let queued = activity_value(
        ctx, build.plan, build.call, None, &fn_scope, &mut stmts, false,
    )?;
    let ran = call_rt(
        ctx,
        RuntimeFn::WfRun,
        vec![Value::Var(queued)],
        &mut stmts,
        build.call.name_span,
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapActivityError,
        vec![Value::Var(ran)],
        &mut stmts,
        build.call.name_span,
    );
    let bound = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: bound,
        result: mapped,
        live_after: LiveAfter::default(),
        span: Span::from_source(build.span),
    });
    let consed = ctx.fresh_var();
    stmts.push(Stmt::ListPrepend {
        dst: consed,
        head: Value::Var(bound),
        tail: Value::Var(acc),
        span: Span::from_source(build.span),
    });
    let ok = ctx.atom("ok");
    let ok_var = record_new(ctx, ok, vec![Value::Var(consed)], &mut stmts);
    fork_fn(
        build,
        (params, param_tys),
        TyDesc::Result(Box::new(acc_desc), Box::new(TyDesc::AwlError)),
        super::super::ops::Block {
            stmts,
            tail: Tail::Return(Value::Var(ok_var)),
        },
    )
}
