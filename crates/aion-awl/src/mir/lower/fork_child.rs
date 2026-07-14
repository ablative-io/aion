//! Child collection-fork lowering, matching the reference emitter's string-name
//! spawn ABI. Parallel forks complete a spawn fold before beginning a second,
//! ordered await fold; sequential forks use `spawn_and_wait` in one fold.

use crate::ast::{Call, ForkStmt, Step};
use crate::emitter::{GType, snake, type_ref_to_g};

use super::super::func::{CodecRef, FlowFn, FnOrigin, MirFn};
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{JsonVal, LiveAfter, Stmt, Tail, ToJsonRef, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::{Leaf, TyDesc};
use super::activity::{call_rt, record_new};
use super::build::{FnPlan, child_output_codec_ref_for, codec_ref_for};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_arg_for};
use super::slots::Slots;

pub(super) struct ChildFork<'a> {
    pub(super) step: &'a Step,
    pub(super) fork: &'a ForkStmt,
    pub(super) call: &'a Call,
    pub(super) var: &'a str,
    pub(super) items: Value,
    pub(super) elem_ty: &'a GType,
    pub(super) returns: &'a GType,
    pub(super) free: &'a [String],
    pub(super) sequential: bool,
}

struct ChildBuild<'a> {
    plan: &'a FnPlan,
    fork: &'a ChildFork<'a>,
    host_scope: &'a Scope,
    ordinal: usize,
}

pub(super) fn lower_child_collection(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    fork: &ChildFork<'_>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    if fork.sequential {
        lower_sequential(ctx, plan, fork, scope, stmts, slots)
    } else {
        lower_parallel(ctx, plan, fork, scope, stmts, slots)
    }
}

fn lower_sequential(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    fork: &ChildFork<'_>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let (ordinal, reference) = slots.forks.take()?;
    let build = ChildBuild {
        plan,
        fork,
        host_scope: scope,
        ordinal,
    };
    let saved = ctx.swap_var_counter(0);
    let function = build_sequential_body(ctx, &build);
    ctx.swap_var_counter(saved);
    slots.forks.finish(ordinal, MirFn::Flow(function?));

    let closure = host_closure(ctx, fork, scope, reference, stmts)?;
    let folded_result = call_rt(
        ctx,
        RuntimeFn::LTryFold,
        vec![fork.items.clone(), Value::Nil, Value::Var(closure)],
        stmts,
        fork.fork.span,
    );
    let folded = try_bind(ctx, folded_result, stmts, fork.fork.span);
    Ok(call_rt(
        ctx,
        RuntimeFn::LReverse,
        vec![Value::Var(folded)],
        stmts,
        fork.fork.span,
    ))
}

fn lower_parallel(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    fork: &ChildFork<'_>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let (spawn_ordinal, spawn_ref) = slots.forks.take()?;
    let spawn_build = ChildBuild {
        plan,
        fork,
        host_scope: scope,
        ordinal: spawn_ordinal,
    };
    let saved = ctx.swap_var_counter(0);
    let spawn_fn = build_spawn_body(ctx, &spawn_build);
    ctx.swap_var_counter(saved);
    slots.forks.finish(spawn_ordinal, MirFn::Flow(spawn_fn?));

    let (await_ordinal, await_ref) = slots.forks.take()?;
    let saved = ctx.swap_var_counter(0);
    let await_fn = build_await_body(ctx, fork, await_ordinal);
    ctx.swap_var_counter(saved);
    slots.forks.finish(await_ordinal, MirFn::Flow(await_fn?));

    // The first fold must finish before the second closure/fold is reached:
    // every child has a distinct spawned handle before any await begins.
    let spawn_closure = host_closure(ctx, fork, scope, spawn_ref, stmts)?;
    let handles_result = call_rt(
        ctx,
        RuntimeFn::LTryFold,
        vec![fork.items.clone(), Value::Nil, Value::Var(spawn_closure)],
        stmts,
        fork.fork.span,
    );
    let handles_reversed = try_bind(ctx, handles_result, stmts, fork.fork.span);

    let await_closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: await_closure,
        lifted: await_ref,
        captures: Vec::new(),
        span: Span::from_source(fork.fork.span),
    });
    let children_result = call_rt(
        ctx,
        RuntimeFn::LTryFold,
        vec![
            Value::Var(handles_reversed),
            Value::Nil,
            Value::Var(await_closure),
        ],
        stmts,
        fork.fork.span,
    );
    Ok(try_bind(ctx, children_result, stmts, fork.fork.span))
}

fn host_closure(
    ctx: &mut Ctx<'_>,
    fork: &ChildFork<'_>,
    scope: &Scope,
    reference: FnRef,
    stmts: &mut Vec<Stmt>,
) -> Result<Var, LowerError> {
    let mut captures = Vec::new();
    for name in fork.free {
        let binding = scope.get(name).ok_or_else(|| {
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
        lifted: reference,
        captures,
        span: Span::from_source(fork.fork.span),
    });
    Ok(closure)
}

fn try_bind(ctx: &mut Ctx<'_>, result: Var, stmts: &mut Vec<Stmt>, span: crate::Span) -> Var {
    let dst = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst,
        result,
        live_after: LiveAfter::default(),
        span: Span::from_source(span),
    });
    dst
}

fn closure_frame(
    ctx: &mut Ctx<'_>,
    build: &ChildBuild<'_>,
    leading: &[(Var, TyDesc)],
) -> Result<(Vec<Var>, Vec<TyDesc>, Scope), LowerError> {
    let mut params = Vec::new();
    let mut param_tys = Vec::new();
    for (var, ty) in leading {
        params.push(*var);
        param_tys.push(ty.clone());
    }
    let mut fn_scope = Scope::new();
    for name in build.fork.free {
        let host = build.host_scope.get(name).ok_or_else(|| {
            LowerError::new(
                build.fork.fork.span,
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
    fork: &ChildFork<'_>,
    ordinal: usize,
    frame: (Vec<Var>, Vec<TyDesc>),
    ret_ty: TyDesc,
    body: super::super::ops::Block,
) -> Result<FlowFn, LowerError> {
    let index = u32::try_from(ordinal).map_err(|_| LowerError::Planning {
        message: "fork ordinal exceeds u32".to_owned(),
    })?;
    let (params, param_tys) = frame;
    Ok(FlowFn {
        origin: FnOrigin::Fork {
            step: fork.step.name.clone(),
            index,
        },
        name: format!("{}_fork_{ordinal}", snake(&fork.step.name)),
        params,
        param_tys,
        ret_ty,
        body,
        span: Span::from_source(fork.fork.span),
        degraded_parallel: false,
    })
}

fn build_sequential_body(ctx: &mut Ctx<'_>, build: &ChildBuild<'_>) -> Result<FlowFn, LowerError> {
    let acc = ctx.fresh_var();
    let item = ctx.fresh_var();
    let results = result_list_desc(ctx, build.fork);
    let (params, param_tys, mut scope) = closure_frame(
        ctx,
        build,
        &[
            (acc, results.clone()),
            (item, ctx.tydesc(build.fork.elem_ty)),
        ],
    )?;
    bind_item(build.fork, item, &mut scope);
    let mut stmts = Vec::new();
    let args = child_spawn_args(ctx, build.plan, build.fork, &scope, &mut stmts)?;
    let waited = call_rt(
        ctx,
        RuntimeFn::WfSpawnAndWait,
        args,
        &mut stmts,
        build.fork.call.name_span,
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapChildError,
        vec![Value::Var(waited)],
        &mut stmts,
        build.fork.call.name_span,
    );
    let item_result = try_bind(ctx, mapped, &mut stmts, build.fork.call.name_span);
    let ok = prepend_ok(ctx, item_result, acc, &mut stmts, build.fork.fork.span);
    fork_fn(
        build.fork,
        build.ordinal,
        (params, param_tys),
        TyDesc::Result(Box::new(results), Box::new(TyDesc::AwlError)),
        super::super::ops::Block {
            stmts,
            tail: Tail::Return(Value::Var(ok)),
        },
    )
}

fn build_spawn_body(ctx: &mut Ctx<'_>, build: &ChildBuild<'_>) -> Result<FlowFn, LowerError> {
    let acc = ctx.fresh_var();
    let item = ctx.fresh_var();
    let handles = TyDesc::List(Box::new(child_handle_desc(ctx, build.fork)));
    let (params, param_tys, mut scope) = closure_frame(
        ctx,
        build,
        &[
            (acc, handles.clone()),
            (item, ctx.tydesc(build.fork.elem_ty)),
        ],
    )?;
    bind_item(build.fork, item, &mut scope);
    let mut stmts = Vec::new();
    let args = child_spawn_args(ctx, build.plan, build.fork, &scope, &mut stmts)?;
    let spawned = call_rt(
        ctx,
        RuntimeFn::WfSpawn,
        args,
        &mut stmts,
        build.fork.call.name_span,
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapSpawnError,
        vec![Value::Var(spawned)],
        &mut stmts,
        build.fork.call.name_span,
    );
    let handle = try_bind(ctx, mapped, &mut stmts, build.fork.call.name_span);
    let ok = prepend_ok(ctx, handle, acc, &mut stmts, build.fork.fork.span);
    fork_fn(
        build.fork,
        build.ordinal,
        (params, param_tys),
        TyDesc::Result(Box::new(handles), Box::new(TyDesc::AwlError)),
        super::super::ops::Block {
            stmts,
            tail: Tail::Return(Value::Var(ok)),
        },
    )
}

fn build_await_body(
    ctx: &mut Ctx<'_>,
    fork: &ChildFork<'_>,
    ordinal: usize,
) -> Result<FlowFn, LowerError> {
    let acc = ctx.fresh_var();
    let handle = ctx.fresh_var();
    let results = result_list_desc(ctx, fork);
    let params = vec![acc, handle];
    let param_tys = vec![results.clone(), child_handle_desc(ctx, fork)];
    let mut stmts = Vec::new();
    let waited = call_rt(
        ctx,
        RuntimeFn::ChildAwait,
        vec![Value::Var(handle)],
        &mut stmts,
        fork.call.name_span,
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapChildError,
        vec![Value::Var(waited)],
        &mut stmts,
        fork.call.name_span,
    );
    let item = try_bind(ctx, mapped, &mut stmts, fork.call.name_span);
    let ok = prepend_ok(ctx, item, acc, &mut stmts, fork.fork.span);
    fork_fn(
        fork,
        ordinal,
        (params, param_tys),
        TyDesc::Result(Box::new(results), Box::new(TyDesc::AwlError)),
        super::super::ops::Block {
            stmts,
            tail: Tail::Return(Value::Var(ok)),
        },
    )
}

fn bind_item(fork: &ChildFork<'_>, item: Var, scope: &mut Scope) {
    scope.insert(
        fork.var.to_owned(),
        Binding {
            var: item,
            ty: fork.elem_ty.clone(),
        },
    );
}

fn prepend_ok(
    ctx: &mut Ctx<'_>,
    item: Var,
    acc: Var,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Var {
    let consed = ctx.fresh_var();
    stmts.push(Stmt::ListPrepend {
        dst: consed,
        head: Value::Var(item),
        tail: Value::Var(acc),
        span: Span::from_source(span),
    });
    let ok = ctx.atom("ok");
    record_new(ctx, ok, vec![Value::Var(consed)], stmts)
}

fn child_spawn_args(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    fork: &ChildFork<'_>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<Vec<Value>, LowerError> {
    let child = ctx
        .emitter
        .children
        .get(fork.call.name.as_str())
        .ok_or_else(|| LowerError::new(fork.call.name_span, "child declaration disappeared"))?;
    let params = child.params.clone();
    let mut pairs = Vec::new();
    for param in &params {
        let arg = fork
            .call
            .args
            .iter()
            .find(|arg| arg.name == param.name)
            .ok_or_else(|| {
                LowerError::new(
                    fork.call.span,
                    format!("call misses argument `{}`", param.name),
                )
            })?;
        let ty = type_ref_to_g(&param.ty);
        let value = lower_arg_for(ctx, &arg.value, &ty, scope, stmts)?;
        pairs.push((
            param.name.clone(),
            JsonVal::Encoded {
                value,
                via: to_json_ref(ctx, plan, &ty)?,
            },
        ));
    }
    let input = ctx.fresh_var();
    stmts.push(Stmt::JsonObj {
        dst: input,
        pairs,
        span: Span::from_source(fork.call.span),
    });
    let witness_ref = plan.child_witness.ok_or_else(|| LowerError::Planning {
        message: "child collection fork has no planned witness".to_owned(),
    })?;
    let witness = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: witness,
        lifted: witness_ref,
        captures: Vec::new(),
        span: Span::from_source(fork.call.name_span),
    });
    let input_codec = call_rt(
        ctx,
        RuntimeFn::JsonValueCodec,
        Vec::new(),
        stmts,
        fork.call.name_span,
    );
    let output_codec_ref = child_output_codec_ref_for(ctx, plan, fork.returns)?;
    let output_codec = codec_value(ctx, &output_codec_ref, stmts, fork.call.name_span);
    let error_codec = call_rt(
        ctx,
        RuntimeFn::ErrCodec,
        Vec::new(),
        stmts,
        fork.call.name_span,
    );
    let name = ctx.binary(&fork.call.name);
    Ok(vec![
        Value::Lit(name),
        Value::Var(witness),
        Value::Var(input),
        Value::Var(input_codec),
        Value::Var(output_codec),
        Value::Var(error_codec),
    ])
}

fn to_json_ref(ctx: &Ctx<'_>, plan: &FnPlan, ty: &GType) -> Result<ToJsonRef, LowerError> {
    match codec_ref_for(ctx, plan, ty)? {
        CodecRef::SdkLeaf(leaf) => Ok(ToJsonRef::SdkLeaf(leaf)),
        CodecRef::Local(reference) => Ok(ToJsonRef::Local(FnRef(reference.0 + 1))),
        CodecRef::SdkNil => Ok(ToJsonRef::SdkLeaf(Leaf::Nil)),
    }
}

fn codec_value(
    ctx: &mut Ctx<'_>,
    codec: &CodecRef,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Var {
    match codec {
        CodecRef::Local(reference) => {
            let dst = ctx.fresh_var();
            stmts.push(Stmt::CallLocal {
                dst: Some(dst),
                callee: *reference,
                args: Vec::new(),
                live_after: LiveAfter::default(),
                span: Span::from_source(span),
            });
            dst
        }
        CodecRef::SdkNil => call_rt(ctx, RuntimeFn::NilCodec, Vec::new(), stmts, span),
        CodecRef::SdkLeaf(leaf) => {
            call_rt(ctx, RuntimeFn::LeafCodec(*leaf), Vec::new(), stmts, span)
        }
    }
}

fn result_list_desc(ctx: &Ctx<'_>, fork: &ChildFork<'_>) -> TyDesc {
    TyDesc::List(Box::new(ctx.tydesc(fork.returns)))
}

fn child_handle_desc(ctx: &Ctx<'_>, fork: &ChildFork<'_>) -> TyDesc {
    TyDesc::ChildHandle(
        Box::new(ctx.tydesc(fork.returns)),
        Box::new(TyDesc::AwlError),
    )
}
