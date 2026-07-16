//! Route and payload lowering — the MIR twin of the reference
//! `emitter/outcomes.rs::emit_route`/`render_payload`: a member flow's exit
//! resolves first (routing to the region's close step returns `Ok(<collected
//! binding>)`), workflow-outcome returns (`Ok(Ctor(payload))` /
//! `Error(AwlOutcomeFailure(…))`) apply only to the host flow, then step
//! routes lower as region tail calls. Payloads: constructed named fields,
//! the piped value, the binding named after the destination, or `Nil` —
//! value payloads (`route out(<value>)`) stay behind the driver's staging
//! gate and refuse here in depth.

use crate::RouteDirection;
use crate::ast::{RoutePayload, RouteTarget};
use crate::emitter::{GType, snake};

use super::super::ops::{Stmt, Tail, Value};
use super::super::runtime::RuntimeFn;
use super::activity::{call_rt, encode_json, record_new};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Scope, lower_arg_for};
use super::flow::FlowEnv;

pub(super) fn route_tail(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    target: &RouteTarget,
    scope: &Scope,
    piped: Option<(Value, GType)>,
    stmts: &mut Vec<Stmt>,
) -> Result<Tail, LowerError> {
    let flow = env.flow;
    // The member flow's exit resolves first (the reference `emit_route`).
    if let Some(exit) = &flow.exit
        && exit.name == target.name
    {
        if piped.is_some() || target.payload.is_some() {
            return Err(LowerError::new(
                target.name_span,
                "routing to a region's `collect` carries no payload — the collect \
                 gathers the per-instance binding",
            ));
        }
        let bound = scope.get(&exit.binding).ok_or_else(|| {
            LowerError::new(
                target.name_span,
                format!(
                    "the collected binding `{}` is not in scope at the exit route",
                    exit.binding
                ),
            )
        })?;
        let ok = ctx.atom("ok");
        let value = record_new(ctx, ok, vec![Value::Var(bound.var)], stmts);
        return Ok(Tail::Return(Value::Var(value)));
    }
    if flow.exit.is_none()
        && let Some(info) = ctx.emitter.outcomes.get(target.name.as_str()).cloned()
    {
        let payload = route_payload(ctx, target, &info.ty, scope, piped, stmts)?;
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
                let json = encode_json(ctx, env.plan, &info.ty, payload, stmts)?;
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
    // A route to another step of this flow: a tail call to its region.
    let step_index = flow
        .steps
        .iter()
        .position(|step| step.name == target.name)
        .ok_or_else(|| {
            LowerError::new(
                target.name_span,
                format!("`{}` names no outcome or step", target.name),
            )
        })?;
    if piped.is_some() {
        return Err(LowerError::new(
            target.name_span,
            "a piped route must target a workflow outcome — steps carry no payloads",
        ));
    }
    if target.payload.is_some() {
        return Err(LowerError::new(
            target.name_span,
            "routing to a step carries no payload",
        ));
    }
    let region = flow
        .plan
        .region_of_entry(step_index)
        .ok_or_else(|| LowerError::unsupported("route to a mid-chain step", target.name_span))?;
    let names = flow.plan.region_params(region).to_vec();
    let mut args = Vec::new();
    for name in &names {
        let binding = scope.get(name).ok_or_else(|| {
            LowerError::new(target.name_span, format!("route needs `{name}` in scope"))
        })?;
        args.push(Value::Var(binding.var));
    }
    Ok(Tail::TailLocal {
        callee: flow.fns.regions[region],
        args,
    })
}

/// Render the payload value a route carries toward a typed destination
/// (the reference `render_payload`): constructed named fields, the piped
/// value, the binding named after the destination, or `Nil`.
fn route_payload(
    ctx: &mut Ctx<'_>,
    target: &RouteTarget,
    outcome_ty: &GType,
    scope: &Scope,
    piped: Option<(Value, GType)>,
    stmts: &mut Vec<Stmt>,
) -> Result<Value, LowerError> {
    if let Some(RoutePayload::Value(value)) = &target.payload {
        // Defense in depth behind the driver's staging gate.
        return Err(LowerError::unsupported(
            "value route payloads (`route out(<value>)`) — the rev-3 flow shape is \
             not yet lowered (B4)",
            crate::spanned::Spanned::span(value),
        ));
    }
    if let Some(RoutePayload::Args(args)) = &target.payload {
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
