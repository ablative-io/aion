//! Activity-call emission for the BC-2 covered subset: the wrapper call, the
//! action-declared config pipe (retry/timeout/`task_queue`/node), the durable
//! `workflow.run` + `map_activity_error` + `TryBind`, `sleep`, and the shared
//! emission primitives (`call_rt`, `record_new`, JSON encode) the routing
//! side of `flow` reuses.

use crate::ast::Call;
use crate::emitter::{GType, type_ref_to_g};

use super::super::func::CodecRef;
use super::super::ids::{AtomRef, FnRef, Span, Var};
use super::super::ops::{LiveAfter, Stmt, ToJsonRef, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::Leaf;
use super::build::{FnPlan, codec_ref_for};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Scope, lower_arg_for, wrap_optional_value};

pub(super) fn zero_span() -> Span {
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

/// Build the UNRUN configured activity value: `<action>_activity(args) |>
/// config` (retry/timeout/`task_queue`/node), without `workflow.run`. This is
/// the value `workflow.map`/`workflow.all` fan-outs take directly; `raw`
/// selects the wire-identical raw wrapper twin (`Activity(String, String)`)
/// heterogeneous named forks dispatch through. `piped` supplies the single
/// argument for a pipe stage; otherwise arguments come from `call.args`.
pub(super) fn activity_value(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    call: &Call,
    piped: Option<(Value, GType)>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    raw: bool,
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
        let wrapped = wrap_optional_value(ctx, value, &value_ty, &expected, stmts, call.name_span);
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
    let wrapper = if raw {
        *plan
            .raw_activities
            .get(call.name.as_str())
            .ok_or_else(|| LowerError::Planning {
                message: format!("raw wrapper for `{}` was never planned", call.name),
            })?
    } else {
        plan.activities[call.name.as_str()]
    };
    let activity = ctx.fresh_var();
    stmts.push(Stmt::CallLocal {
        dst: Some(activity),
        callee: wrapper,
        args: arg_values,
        live_after: LiveAfter::default(),
        span: Span::from_source(call.name_span),
    });
    apply_action_config(
        ctx,
        config.as_ref(),
        activity,
        &queue,
        stmts,
        call.name_span,
    )
}

/// Build the activity call: `<action>_activity(args) |> config |>
/// workflow.run |> map_activity_error`, then `TryBind`. `piped` supplies the
/// single argument for a pipe stage; otherwise arguments come from `call.args`.
pub(super) fn activity_call(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    call: &Call,
    piped: Option<(Value, GType)>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<Var, LowerError> {
    let queued = activity_value(ctx, plan, call, piped, scope, stmts, false)?;
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

pub(super) fn call_rt(
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

pub(super) fn lower_sleep(
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

/// Encode a payload to JSON through its output codec's `_to_json` (or a leaf).
pub(super) fn encode_json(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    ty: &GType,
    payload: Value,
    stmts: &mut Vec<Stmt>,
) -> Result<Var, LowerError> {
    let via = match codec_ref_for(ctx, plan, ty)? {
        CodecRef::SdkLeaf(leaf) => ToJsonRef::SdkLeaf(leaf),
        // The `_to_json` fn sits one slot after `_codec`.
        CodecRef::Local(codec_ref) => ToJsonRef::Local(FnRef(codec_ref.0 + 1)),
        CodecRef::SdkNil => ToJsonRef::SdkLeaf(Leaf::Nil),
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
    Ok(dst)
}

pub(super) fn record_new(
    ctx: &mut Ctx<'_>,
    tag: AtomRef,
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
