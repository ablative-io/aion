//! Region-body lowering for the BC-2 covered subset: sequential regions whose
//! step bodies are action calls, sleeps, and pipe chains (action/field stages)
//! ending in a route. A multi-step chain lowers as one `FlowFn` per step, each
//! non-terminal step ending in a tail call to its successor (IR-14) with the
//! chain-boundary live set (`chain::chain_params`) as arguments. Outcome
//! cascades, forks, loops, substeps, waits, spawns, `on failure`, combinators,
//! and dependency-parallel layers are deferred (`LowerError::unsupported`) —
//! visible incompleteness, never silent divergence from the reference. The
//! activity-emission primitives live in `activity`.

use crate::RouteDirection;
use crate::ast::{Call, CallStmt, PipeEnd, PipeStage, RouteTarget, Statement, Step};
use crate::emitter::{GType, snake, type_ref_to_g};

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{Block, Stmt, Tail, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::{activity_call, call_rt, encode_json, lower_sleep, record_new, zero_span};
use super::build::{FnPlan, output_tydesc};
use super::chain::chain_params;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_arg_for, lower_expr};

/// The fall-through continuation of a non-terminal chain step: the successor's
/// function ref and parameter names.
struct Next {
    callee: FnRef,
    param_names: Vec<String>,
}

/// Lower every region into `step_<name>` `FlowFn`s (one per chain step, in
/// chain order), appended to `functions`.
pub(super) fn lower_regions(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    functions: &mut Vec<MirFn>,
) -> Result<(), LowerError> {
    for region_index in 0..ctx.plan.regions.len() {
        lower_region(ctx, plan, region_index, functions)?;
    }
    Ok(())
}

fn lower_region(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    region_index: usize,
    functions: &mut Vec<MirFn>,
) -> Result<(), LowerError> {
    let region = &ctx.plan.regions[region_index];
    let entry_index = region.entry;
    let layers = region.layers.clone();
    let entry_step = ctx.emitter.document.steps[entry_index].clone();

    // A sequential chain has exactly one member per layer; dependency-parallel
    // layers stay deferred.
    let mut chain = Vec::with_capacity(layers.len());
    for layer in &layers {
        let [member] = layer.as_slice() else {
            return Err(LowerError::unsupported(
                "parallel region",
                entry_step.name_span,
            ));
        };
        chain.push(*member);
    }
    if chain.first() != Some(&entry_index) {
        return Err(LowerError::unsupported(
            "parallel region",
            entry_step.name_span,
        ));
    }

    let params = chain_params(ctx.emitter, ctx.plan, &chain);
    for (position, &step_index) in chain.iter().enumerate() {
        let step = ctx.emitter.document.steps[step_index].clone();
        // The entry's parameter list is the shared plan's fixed point (the
        // parity anchor); chain boundaries use the backward live sets.
        let param_names = if position == 0 {
            ctx.plan.region_params(region_index).to_vec()
        } else {
            params[position].clone()
        };
        let next = chain.get(position + 1).map(|_| Next {
            callee: plan.chains[region_index][position + 1],
            param_names: params[position + 1].clone(),
        });
        let flow = lower_chain_step(ctx, plan, &entry_step, &step, position, &param_names, next)?;
        functions.push(MirFn::Flow(flow));
    }
    Ok(())
}

fn lower_chain_step(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    entry_step: &Step,
    step: &Step,
    position: usize,
    param_names: &[String],
    next: Option<Next>,
) -> Result<FlowFn, LowerError> {
    ctx.reset_vars();
    let mut scope: Scope = Scope::new();
    let mut param_vars = Vec::new();
    let mut param_tys = Vec::new();
    for name in param_names {
        let ty = ctx.emitter.bindings.get(name).cloned().ok_or_else(|| {
            LowerError::new(step.name_span, format!("binding `{name}` has no type"))
        })?;
        let var = ctx.fresh_var();
        param_vars.push(var);
        param_tys.push(ctx.tydesc(&ty));
        scope.insert(name.clone(), Binding { var, ty });
    }

    let body = lower_step(ctx, plan, step, &mut scope, next)?;
    let origin = if position == 0 {
        FnOrigin::Region {
            entry_step: entry_step.name.clone(),
        }
    } else {
        FnOrigin::ChainStep {
            entry_step: entry_step.name.clone(),
            step: step.name.clone(),
        }
    };
    Ok(FlowFn {
        origin,
        name: format!("step_{}", snake(&step.name)),
        params: param_vars,
        param_tys,
        ret_ty: TyDesc::Result(Box::new(output_tydesc(ctx)), Box::new(TyDesc::AwlError)),
        body,
        span: Span::from_source(step.name_span),
        degraded_parallel: false,
    })
}

fn lower_step(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    scope: &mut Scope,
    next: Option<Next>,
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
    // Fall-through: hand the chain-boundary live set to the successor as a
    // tail call (IR-14).
    if let Some(next) = next {
        let mut args = Vec::new();
        for name in &next.param_names {
            let binding = scope.get(name).ok_or_else(|| {
                LowerError::new(
                    step.name_span,
                    format!("fall-through needs `{name}` in scope"),
                )
            })?;
            args.push(Value::Var(binding.var));
        }
        return Ok(Block {
            stmts,
            tail: Tail::TailLocal {
                callee: next.callee,
                args,
            },
        });
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
