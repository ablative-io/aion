//! Region-body lowering for the BC-2 covered subset: single-step regions whose
//! bodies are action calls, sleeps, and pipe chains (action/field stages)
//! ending in a route. Outcome cascades, forks, loops, substeps, waits, spawns,
//! `on failure`, and combinators are deferred (`LowerError::unsupported`) —
//! visible incompleteness, never silent divergence from the reference.

use crate::RouteDirection;
use crate::ast::{Call, CallStmt, PipeEnd, PipeStage, RouteTarget, Statement, Step};
use crate::emitter::{GType, snake, type_ref_to_g};

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{Span, Var};
use super::super::ops::{Block, LiveAfter, Stmt, Tail, ToJsonRef, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::build::{FnPlan, codec_ref_for, output_tydesc};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_arg_for, lower_expr};

fn zero_span() -> Span {
    Span::zero()
}

fn zero_src() -> crate::Span {
    crate::Span {
        start: 0,
        end: 0,
        line: 0,
        column: 0,
    }
}

/// Lower every region into a `step_<entry>` `FlowFn`, appended to `functions`.
pub(super) fn lower_regions(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    functions: &mut Vec<MirFn>,
) -> Result<(), LowerError> {
    for region_index in 0..ctx.plan.regions.len() {
        let flow = lower_region(ctx, plan, region_index)?;
        functions.push(MirFn::Flow(flow));
    }
    Ok(())
}

fn lower_region(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    region_index: usize,
) -> Result<FlowFn, LowerError> {
    let region = &ctx.plan.regions[region_index];
    let entry_index = region.entry;
    let layers = region.layers.clone();
    let entry_step = ctx.emitter.document.steps[entry_index].clone();
    let param_names = ctx.plan.region_params(region_index).to_vec();

    if layers.len() != 1 || layers[0].len() != 1 || layers[0][0] != entry_index {
        return Err(LowerError::unsupported(
            "multi-step or parallel region",
            entry_step.name_span,
        ));
    }

    ctx.reset_vars();
    let mut scope: Scope = Scope::new();
    let mut param_vars = Vec::new();
    let mut param_tys = Vec::new();
    for name in &param_names {
        let ty = ctx.emitter.bindings.get(name).cloned().ok_or_else(|| {
            LowerError::new(
                entry_step.name_span,
                format!("binding `{name}` has no type"),
            )
        })?;
        let var = ctx.fresh_var();
        param_vars.push(var);
        param_tys.push(ctx.tydesc(&ty));
        scope.insert(name.clone(), Binding { var, ty });
    }

    let body = lower_step(ctx, plan, &entry_step, &mut scope)?;
    Ok(FlowFn {
        origin: FnOrigin::Region {
            entry_step: entry_step.name.clone(),
        },
        name: format!("step_{}", snake(&entry_step.name)),
        params: param_vars,
        param_tys,
        ret_ty: TyDesc::Result(Box::new(output_tydesc(ctx)), Box::new(TyDesc::AwlError)),
        body,
        span: Span::from_source(entry_step.name_span),
        degraded_parallel: false,
    })
}

fn lower_step(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    scope: &mut Scope,
) -> Result<Block, LowerError> {
    if step.on_failure.is_some() {
        return Err(LowerError::unsupported("on failure", step.name_span));
    }
    if step.body.iter().any(|s| matches!(s, Statement::SubStep(_))) {
        return Err(LowerError::unsupported("substeps", step.name_span));
    }
    let mut stmts = Vec::new();
    for statement in &step.body {
        if let Some(tail) = lower_statement(ctx, plan, statement, scope, &mut stmts)? {
            return Ok(Block { stmts, tail });
        }
    }
    if !step.outcomes.is_empty() {
        return Err(LowerError::unsupported("outcome clauses", step.name_span));
    }
    Err(LowerError::unsupported(
        "step falls through without a route",
        step.name_span,
    ))
}

fn lower_statement(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    statement: &Statement,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<Option<Tail>, LowerError> {
    match statement {
        Statement::Call(call) => {
            lower_call(ctx, plan, call, scope, stmts)?;
            Ok(None)
        }
        Statement::Sleep(sleep) => {
            lower_sleep(ctx, sleep.duration.magnitude, sleep.duration.unit, stmts);
            Ok(None)
        }
        Statement::Pipe(pipe) => {
            let (value, ty) = lower_pipe_value(ctx, plan, &pipe.head, &pipe.stages, scope, stmts)?;
            match &pipe.end {
                PipeEnd::Bind(binding) => {
                    let var = as_var(ctx, value, stmts);
                    scope.insert(binding.name.clone(), Binding { var, ty });
                    Ok(None)
                }
                PipeEnd::Route(target) => {
                    let tail = route_tail(ctx, plan, target, scope, Some((value, ty)), stmts)?;
                    Ok(Some(tail))
                }
            }
        }
        Statement::Route(route) => {
            let tail = route_tail(ctx, plan, &route.target, scope, None, stmts)?;
            Ok(Some(tail))
        }
        Statement::Spawn(spawn) => Err(LowerError::unsupported("spawn", spawn.span)),
        Statement::Wait(wait) => Err(LowerError::unsupported("wait", wait.span)),
        Statement::Fork(fork) => Err(LowerError::unsupported("fork", fork.span)),
        Statement::Loop(looped) => Err(LowerError::unsupported("loop", looped.span)),
        Statement::SubStep(sub) => Err(LowerError::unsupported("substep", sub.name_span)),
    }
}

fn lower_call(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    call: &CallStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(), LowerError> {
    if call.config.is_some() {
        return Err(LowerError::unsupported("call-site config", call.span));
    }
    let bound = activity_call(ctx, plan, &call.call, None, scope, stmts)?;
    if let Some(bind) = &call.bind {
        let ty = ctx
            .emitter
            .actions
            .get(call.call.name.as_str())
            .map(|&(_, decl)| type_ref_to_g(&decl.returns))
            .ok_or_else(|| LowerError::unsupported("child call", call.call.name_span))?;
        scope.insert(bind.name.clone(), Binding { var: bound, ty });
    }
    Ok(())
}

/// Build the activity call: `<action>_activity(args) |> task_queue |>
/// workflow.run |> map_activity_error`, then `TryBind`. `piped` supplies the
/// single argument for a pipe stage; otherwise arguments come from `call.args`.
fn activity_call(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    call: &Call,
    piped: Option<(Value, GType)>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<Var, LowerError> {
    let Some(&(queue, decl)) = ctx.emitter.actions.get(call.name.as_str()) else {
        return Err(LowerError::unsupported(
            "child call or unknown action",
            call.name_span,
        ));
    };
    let queue = queue.to_owned();
    let params = decl.params.clone();
    let config = decl.config.clone();
    let mut arg_values = Vec::new();
    if let Some((value, value_ty)) = piped {
        let [param] = params.as_slice() else {
            return Err(LowerError::unsupported(
                "multi-arg action in pipe",
                call.name_span,
            ));
        };
        let expected = type_ref_to_g(&param.ty);
        let wrapped = super::expr::wrap_optional_value(
            ctx,
            value,
            &value_ty,
            &expected,
            stmts,
            call.name_span,
        );
        arg_values.push(wrapped);
    } else {
        for param in &params {
            let arg = call
                .args
                .iter()
                .find(|arg| arg.name == param.name)
                .ok_or_else(|| {
                    LowerError::new(call.span, format!("call misses argument `{}`", param.name))
                })?;
            let value = lower_arg_for(ctx, &arg.value, &type_ref_to_g(&param.ty), scope, stmts)?;
            arg_values.push(value);
        }
    }
    let wrapper = plan.activities[call.name.as_str()];
    let activity = ctx.fresh_var();
    stmts.push(Stmt::CallLocal {
        dst: Some(activity),
        callee: wrapper,
        args: arg_values,
        live_after: LiveAfter::default(),
        span: Span::from_source(call.name_span),
    });
    let queued = apply_action_config(
        ctx,
        config.as_ref(),
        activity,
        &queue,
        stmts,
        call.name_span,
    )?;
    let ran = call_rt(
        ctx,
        RuntimeFn::WfRun,
        vec![Value::Var(queued)],
        stmts,
        call.name_span,
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapActivityError,
        vec![Value::Var(ran)],
        stmts,
        call.name_span,
    );
    let bound = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: bound,
        result: mapped,
        live_after: LiveAfter::default(),
        span: Span::from_source(call.name_span),
    });
    Ok(bound)
}

/// Apply action-declared config in the reference's order (`stmts.rs:84-104`):
/// retry, timeout, `task_queue` (always), node. Call-site config is refused
/// upstream, so only the declaration config participates. Returns the final
/// activity value to hand to `workflow.run`.
fn apply_action_config(
    ctx: &mut Ctx<'_>,
    config: Option<&crate::ast::ConfigLine>,
    activity: Var,
    queue: &str,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Result<Var, LowerError> {
    let mut current = activity;
    if let Some(retry) = config.and_then(|config| config.retry.as_ref()) {
        let policy = build_retry_policy(ctx, retry, stmts, span)?;
        current = call_rt(
            ctx,
            RuntimeFn::ActRetry,
            vec![Value::Var(current), Value::Var(policy)],
            stmts,
            span,
        );
    }
    if let Some(timeout) = config.and_then(|config| config.timeout.as_ref()) {
        let dur = duration_var(ctx, timeout.magnitude, timeout.unit, stmts, span);
        current = call_rt(
            ctx,
            RuntimeFn::ActTimeout,
            vec![Value::Var(current), Value::Var(dur)],
            stmts,
            span,
        );
    }
    let queue_lit = ctx.binary(queue);
    let mut queued = ctx.fresh_var();
    stmts.push(Stmt::CallRt {
        dst: Some(queued),
        callee: RuntimeFn::ActTaskQueue,
        args: vec![Value::Var(current), Value::Lit(queue_lit)],
        live_after: LiveAfter::default(),
        span: Span::from_source(span),
    });
    if let Some(node) = config.and_then(|config| config.node.as_ref()) {
        let node_lit = ctx.binary(&node.name);
        queued = call_rt(
            ctx,
            RuntimeFn::ActNode,
            vec![Value::Var(queued), Value::Lit(node_lit)],
            stmts,
            span,
        );
    }
    Ok(queued)
}

/// A `duration.milliseconds(ms)` runtime value for a source duration.
fn duration_var(
    ctx: &mut Ctx<'_>,
    magnitude: u64,
    unit: crate::DurationUnit,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Var {
    let ms = duration_ms(magnitude, unit);
    call_rt(
        ctx,
        RuntimeFn::DurationMs,
        vec![Value::Int(ms)],
        stmts,
        span,
    )
}

/// Build the `activity.RetryPolicy(max_attempts, backoff)` record the SDK's
/// `activity.retry/2` takes, mirroring the reference `retry_policy`
/// (`stmts.rs:141-156`): `Fixed(delay)` or `Exponential(initial, 2.0, max)`.
fn build_retry_policy(
    ctx: &mut Ctx<'_>,
    retry: &crate::ast::RetrySpec,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Result<Var, LowerError> {
    use crate::ast::RetrySpec;
    let (count, backoff) = match retry {
        RetrySpec::Every { count, every, .. } => {
            let delay = duration_var(ctx, every.magnitude, every.unit, stmts, span);
            let fixed = ctx.atom("fixed");
            (
                *count,
                record_new(ctx, fixed, vec![Value::Var(delay)], stmts),
            )
        }
        RetrySpec::Backoff {
            count, min, max, ..
        } => {
            let initial = duration_var(ctx, min.magnitude, min.unit, stmts, span);
            let multiplier = ctx.push_float("2.0");
            let cap = duration_var(ctx, max.magnitude, max.unit, stmts, span);
            let exponential = ctx.atom("exponential");
            (
                *count,
                record_new(
                    ctx,
                    exponential,
                    vec![Value::Var(initial), Value::Lit(multiplier), Value::Var(cap)],
                    stmts,
                ),
            )
        }
    };
    let count_int = i64::try_from(count)
        .map_err(|_| LowerError::unsupported("retry count above i64::MAX", span))?;
    let policy_tag = ctx.atom("retry_policy");
    Ok(record_new(
        ctx,
        policy_tag,
        vec![Value::Int(count_int), Value::Var(backoff)],
        stmts,
    ))
}

fn call_rt(
    ctx: &mut Ctx<'_>,
    callee: RuntimeFn,
    args: Vec<Value>,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Var {
    let dst = ctx.fresh_var();
    stmts.push(Stmt::CallRt {
        dst: Some(dst),
        callee,
        args,
        live_after: LiveAfter::default(),
        span: Span::from_source(span),
    });
    dst
}

fn lower_sleep(
    ctx: &mut Ctx<'_>,
    magnitude: u64,
    unit: crate::DurationUnit,
    stmts: &mut Vec<Stmt>,
) {
    let ms = duration_ms(magnitude, unit);
    let millis = call_rt(
        ctx,
        RuntimeFn::DurationMs,
        vec![Value::Int(ms)],
        stmts,
        zero_src(),
    );
    let slept = call_rt(
        ctx,
        RuntimeFn::WfSleep,
        vec![Value::Var(millis)],
        stmts,
        zero_src(),
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapTimerError,
        vec![Value::Var(slept)],
        stmts,
        zero_src(),
    );
    let discard = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: discard,
        result: mapped,
        live_after: LiveAfter::default(),
        span: zero_span(),
    });
}

fn duration_ms(magnitude: u64, unit: crate::DurationUnit) -> i64 {
    let ms = match unit {
        crate::DurationUnit::Seconds => magnitude.saturating_mul(1_000),
        crate::DurationUnit::Minutes => magnitude.saturating_mul(60_000),
        crate::DurationUnit::Hours => magnitude.saturating_mul(3_600_000),
        crate::DurationUnit::Days => magnitude.saturating_mul(86_400_000),
    };
    i64::try_from(ms).unwrap_or(i64::MAX)
}

fn lower_pipe_value(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    head: &crate::ast::Expr,
    stages: &[PipeStage],
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(Value, GType), LowerError> {
    let (mut value, mut ty) = lower_expr(ctx, head, scope, stmts)?;
    for stage in stages {
        match stage {
            PipeStage::Action { name, span } => {
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
                return Err(LowerError::unsupported("pipe combinator", combinator.span));
            }
        }
    }
    Ok((value, ty))
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

fn route_tail(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    target: &RouteTarget,
    scope: &Scope,
    piped: Option<(Value, GType)>,
    stmts: &mut Vec<Stmt>,
) -> Result<Tail, LowerError> {
    if let Some(info) = ctx.emitter.outcomes.get(target.name.as_str()).cloned() {
        let payload = outcome_payload(ctx, target, &info.ty, scope, piped, stmts)?;
        return match info.direction {
            RouteDirection::Success => {
                let constructor = info.constructor.ok_or_else(|| {
                    LowerError::new(target.name_span, "success outcome lost its constructor")
                })?;
                let ctor = ctx.atom(&snake(&constructor));
                let wrapped = record_new(ctx, ctor, vec![payload], stmts);
                let ok = ctx.atom("ok");
                let ok_value = record_new(ctx, ok, vec![Value::Var(wrapped)], stmts);
                Ok(Tail::Return(Value::Var(ok_value)))
            }
            RouteDirection::Failure => {
                let json = encode_json(ctx, plan, &info.ty, payload, stmts);
                let string = call_rt(
                    ctx,
                    RuntimeFn::JToString,
                    vec![Value::Var(json)],
                    stmts,
                    target.name_span,
                );
                let name_lit = ctx.binary(&target.name);
                let failure_atom = ctx.atom("awl_outcome_failure");
                let failure = record_new(
                    ctx,
                    failure_atom,
                    vec![Value::Lit(name_lit), Value::Var(string)],
                    stmts,
                );
                let error_atom = ctx.atom("error");
                let error = record_new(ctx, error_atom, vec![Value::Var(failure)], stmts);
                Ok(Tail::Return(Value::Var(error)))
            }
        };
    }
    // A route to another step: a tail call to its region.
    let step_index = ctx
        .emitter
        .document
        .steps
        .iter()
        .position(|step| step.name == target.name)
        .ok_or_else(|| {
            LowerError::new(
                target.name_span,
                format!("`{}` names no outcome or step", target.name),
            )
        })?;
    let region = ctx
        .plan
        .region_of_entry(step_index)
        .ok_or_else(|| LowerError::unsupported("route to a mid-chain step", target.name_span))?;
    let names = ctx.plan.region_params(region).to_vec();
    let mut args = Vec::new();
    for name in &names {
        let binding = scope.get(name).ok_or_else(|| {
            LowerError::new(target.name_span, format!("route needs `{name}` in scope"))
        })?;
        args.push(Value::Var(binding.var));
    }
    Ok(Tail::TailLocal {
        callee: plan.regions[region],
        args,
    })
}

fn outcome_payload(
    ctx: &mut Ctx<'_>,
    target: &RouteTarget,
    outcome_ty: &GType,
    scope: &Scope,
    piped: Option<(Value, GType)>,
    stmts: &mut Vec<Stmt>,
) -> Result<Value, LowerError> {
    if let Some(args) = &target.payload {
        let Some((gleam_name, record)) = ctx.emitter.env.record_of(outcome_ty) else {
            return Err(LowerError::new(
                target.name_span,
                "constructed payload needs a record outcome",
            ));
        };
        let fields = record.fields.clone();
        let tag = ctx.atom(&snake(&gleam_name));
        if fields.is_empty() {
            return Ok(Value::Atom(tag));
        }
        let mut values = Vec::new();
        for field in &fields {
            let value = match args.iter().find(|arg| arg.name == field.awl_name) {
                Some(arg) => lower_arg_for(ctx, &arg.value, &field.ty, scope, stmts)?,
                None if matches!(ctx.emitter.env.resolve(&field.ty), GType::Option(_)) => {
                    Value::Atom(ctx.atom("none"))
                }
                None => {
                    return Err(LowerError::new(
                        target.span,
                        format!("outcome misses field `{}`", field.awl_name),
                    ));
                }
            };
            values.push(value);
        }
        return Ok(Value::Var(record_new(ctx, tag, values, stmts)));
    }
    if let Some((value, _)) = piped {
        return Ok(value);
    }
    if let Some(binding) = scope.get(target.name.as_str()) {
        return Ok(Value::Var(binding.var));
    }
    if matches!(ctx.emitter.env.resolve(outcome_ty), GType::Nil) {
        return Ok(Value::Nil);
    }
    Err(LowerError::new(
        target.name_span,
        format!("bare route `{}` needs a binding in scope", target.name),
    ))
}

fn encode_json(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    ty: &GType,
    payload: Value,
    stmts: &mut Vec<Stmt>,
) -> Var {
    let via = match codec_ref_for(ctx, plan, ty) {
        super::super::func::CodecRef::SdkLeaf(leaf) => ToJsonRef::SdkLeaf(leaf),
        super::super::func::CodecRef::Local(codec_ref) => {
            // The `_to_json` fn sits one slot after `_codec`.
            ToJsonRef::Local(super::super::ids::FnRef(codec_ref.0 + 1))
        }
        super::super::func::CodecRef::SdkNil => ToJsonRef::SdkLeaf(super::super::tydesc::Leaf::Nil),
    };
    let dst = ctx.fresh_var();
    match via {
        ToJsonRef::SdkLeaf(leaf) => {
            stmts.push(Stmt::CallRt {
                dst: Some(dst),
                callee: RuntimeFn::LeafToJson(leaf),
                args: vec![payload],
                live_after: LiveAfter::default(),
                span: zero_span(),
            });
        }
        ToJsonRef::Local(reference) => {
            stmts.push(Stmt::CallLocal {
                dst: Some(dst),
                callee: reference,
                args: vec![payload],
                live_after: LiveAfter::default(),
                span: zero_span(),
            });
        }
    }
    dst
}

fn record_new(
    ctx: &mut Ctx<'_>,
    tag: super::super::ids::AtomRef,
    args: Vec<Value>,
    stmts: &mut Vec<Stmt>,
) -> Var {
    let dst = ctx.fresh_var();
    stmts.push(Stmt::RecordNew {
        dst,
        tag,
        args,
        span: zero_span(),
    });
    dst
}

fn as_var(ctx: &mut Ctx<'_>, value: Value, stmts: &mut Vec<Stmt>) -> Var {
    match value {
        Value::Var(var) => var,
        other => {
            let dst = ctx.fresh_var();
            stmts.push(Stmt::Bind {
                dst,
                value: other,
                span: zero_span(),
            });
            dst
        }
    }
}
