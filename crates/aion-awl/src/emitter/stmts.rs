//! Statement lowering: calls, spawn, pipes and combinators, waits, sleeps,
//! forks, and loops. Routes and outcome clauses lower in `steps` (their
//! resolution depends on the step/substep frame).

use std::fmt::Write as _;

use crate::ast::{Arg, Call, CallStmt, ChildDecl, ConfigLine, SleepStmt, SpawnStmt, WaitStmt};
use crate::{RetrySpec, Span};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, duration_expr, render_arg_for};
use super::names::{ident, snake, string_lit};
use super::types::{GType, type_ref_to_g};

/// The witness closure the SDK's string-name child spawn requires: a type
/// anchor the engine never calls (panel-hardened AWL-0 discipline).
pub(super) const CHILD_WITNESS: &str =
    "fn(_: json.Json) { Error(AwlChildFailed(\"child workflow body runs in its own execution\")) }";

/// Emit any pending prelude lines (fallible indexing) before a statement.
pub(super) fn flush_prelude(emitter: &mut Emitter<'_>, prelude: Vec<String>) {
    for line in prelude {
        emitter.line(&line);
    }
}

/// Build the activity-value expression for an action call: the wrapper call
/// with arguments in declared order, then retry/timeout/queue/node config
/// (call-site override > action config; the worker name is the task queue).
pub(super) fn activity_value(
    emitter: &mut Emitter<'_>,
    call: &Call,
    site_config: Option<&ConfigLine>,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    activity_value_form(emitter, call, site_config, scope, prelude, false)
}

/// [`activity_value`] over the raw wrapper twin (`Activity(String, String)`,
/// wire bytes identical) that heterogeneous parallel groups share one
/// `workflow.all` list through; registers the twin for generation.
pub(super) fn activity_value_raw(
    emitter: &mut Emitter<'_>,
    call: &Call,
    site_config: Option<&ConfigLine>,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    activity_value_form(emitter, call, site_config, scope, prelude, true)
}

fn activity_value_form(
    emitter: &mut Emitter<'_>,
    call: &Call,
    site_config: Option<&ConfigLine>,
    scope: &Scope,
    prelude: &mut Vec<String>,
    raw: bool,
) -> Result<String, EmitError> {
    let Some(&(queue, action)) = emitter.actions.get(call.name.as_str()) else {
        return Err(EmitError::new(
            call.name_span,
            format!("`{}` names no declared action", call.name),
        ));
    };
    let (queue, action) = (queue.to_owned(), action);
    let args = ordered_args(&call.args, &action.params, call.span, &call.name)?;
    let mut value = if raw {
        emitter.flags.raw_actions.insert(call.name.clone());
        format!("{}_activity_raw(", snake(&call.name))
    } else {
        format!("{}_activity(", snake(&call.name))
    };
    for (position, (arg, param_ty)) in args.iter().enumerate() {
        if position > 0 {
            value.push_str(", ");
        }
        let rendered = render_arg_for(emitter, &arg.value, param_ty, scope, prelude)?;
        value.push_str(&rendered);
    }
    value.push(')');
    let action_config = action.config.as_ref();
    let retry = site_config
        .and_then(|config| config.retry.as_ref())
        .or_else(|| action_config.and_then(|config| config.retry.as_ref()));
    if let Some(retry) = retry {
        let _ = write!(value, " |> activity.retry({})", retry_policy(retry));
    }
    let timeout = site_config
        .and_then(|config| config.timeout.as_ref())
        .or_else(|| action_config.and_then(|config| config.timeout.as_ref()));
    if let Some(timeout) = timeout {
        let _ = write!(value, " |> activity.timeout({})", duration_expr(timeout));
    }
    let _ = write!(value, " |> activity.task_queue({})", string_lit(&queue));
    let node = site_config
        .and_then(|config| config.node.as_ref())
        .or_else(|| action_config.and_then(|config| config.node.as_ref()));
    if let Some(node) = node {
        let _ = write!(value, " |> activity.node({})", string_lit(&node.name));
    }
    Ok(value)
}

/// Match named call arguments against declared parameters, in declared order.
fn ordered_args<'c>(
    args: &'c [Arg],
    params: &[crate::ast::ParamDecl],
    span: Span,
    callee: &str,
) -> Result<Vec<(&'c Arg, GType)>, EmitError> {
    for arg in args {
        if !params.iter().any(|param| param.name == arg.name) {
            return Err(EmitError::new(
                arg.name_span,
                format!("`{callee}` declares no parameter `{}`", arg.name),
            ));
        }
    }
    params
        .iter()
        .map(|param| {
            args.iter()
                .find(|arg| arg.name == param.name)
                .map(|arg| (arg, type_ref_to_g(&param.ty)))
                .ok_or_else(|| {
                    EmitError::new(
                        span,
                        format!(
                            "call to `{callee}` misses required argument `{}`",
                            param.name
                        ),
                    )
                })
        })
        .collect()
}

pub(super) fn retry_policy(retry: &RetrySpec) -> String {
    match retry {
        RetrySpec::Every { count, every, .. } => format!(
            "activity.RetryPolicy(max_attempts: {count}, backoff: activity.Fixed({}))",
            duration_expr(every)
        ),
        RetrySpec::Backoff {
            count, min, max, ..
        } => format!(
            "activity.RetryPolicy(max_attempts: {count}, backoff: activity.Exponential(initial: \
             {}, multiplier: 2.0, max: {}))",
            duration_expr(min),
            duration_expr(max)
        ),
    }
}

/// Build the string-name child-spawn expression (without the leading
/// `workflow.spawn`/`workflow.spawn_and_wait`, which differs by call form).
fn child_spawn_args(
    emitter: &mut Emitter<'_>,
    child: &ChildDecl,
    call: &Call,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    emitter.flags.uses_child = true;
    let args = ordered_args(&call.args, &child.params, call.span, &call.name)?;
    let mut fields = Vec::new();
    for (arg, param_ty) in args {
        let codec = emitter.env.codec_name(&param_ty);
        let rendered = render_arg_for(emitter, &arg.value, &param_ty, scope, prelude)?;
        fields.push(format!(
            "#({}, {codec}_to_json({rendered}))",
            string_lit(&arg.name)
        ));
    }
    let input = format!("json.object([{}])", fields.join(", "));
    let output_codec = emitter.env.codec_name(&type_ref_to_g(&child.returns));
    Ok(format!(
        "({}, {CHILD_WITNESS}, {input}, json_value_codec(), {output_codec}_codec(), \
         awl_error_codec())",
        string_lit(&call.name)
    ))
}

/// Lower one call statement (action or awaited child).
pub(super) fn lower_call(
    emitter: &mut Emitter<'_>,
    call: &CallStmt,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let mut prelude = Vec::new();
    let binder = call
        .bind
        .as_ref()
        .map_or_else(|| "_".to_owned(), |bind| ident(&bind.name));
    if emitter.actions.contains_key(call.call.name.as_str()) {
        let value = activity_value(
            emitter,
            &call.call,
            call.config.as_ref(),
            scope,
            &mut prelude,
        )?;
        flush_prelude(emitter, prelude);
        emitter.line(&format!(
            "use {binder} <- try({value} |> workflow.run |> map_activity_error)"
        ));
        if let Some(bind) = &call.bind {
            let (_, action) = emitter.actions[call.call.name.as_str()];
            scope.insert(bind.name.clone(), type_ref_to_g(&action.returns));
        }
        return Ok(());
    }
    let Some(&child) = emitter.children.get(call.call.name.as_str()) else {
        return Err(EmitError::new(
            call.call.name_span,
            format!(
                "`{}` names neither a declared action nor a child workflow",
                call.call.name
            ),
        ));
    };
    if call.config.is_some() {
        return Err(EmitError::new(
            call.span,
            "`node`/`timeout` cannot pin a child workflow call — the engine routes children, \
             not a queue",
        ));
    }
    let spawn = child_spawn_args(emitter, child, &call.call, scope, &mut prelude)?;
    flush_prelude(emitter, prelude);
    emitter.line(&format!(
        "use {binder} <- try(workflow.spawn_and_wait{spawn} |> map_child_error)"
    ));
    if let Some(bind) = &call.bind {
        scope.insert(bind.name.clone(), type_ref_to_g(&child.returns));
    }
    Ok(())
}

/// Lower a fire-and-forget `spawn` statement.
pub(super) fn lower_spawn(
    emitter: &mut Emitter<'_>,
    spawn: &SpawnStmt,
    scope: &Scope,
) -> Result<(), EmitError> {
    if let Some(bind) = &spawn.bind {
        return Err(EmitError::new(
            bind.span,
            "`spawn` is fire-and-forget: binding its result is a check error",
        ));
    }
    let Some(&child) = emitter.children.get(spawn.call.name.as_str()) else {
        return Err(EmitError::new(
            spawn.call.name_span,
            format!("`{}` names no declared child workflow", spawn.call.name),
        ));
    };
    let mut prelude = Vec::new();
    let args = child_spawn_args(emitter, child, &spawn.call, scope, &mut prelude)?;
    flush_prelude(emitter, prelude);
    emitter.line(&format!(
        "use _ <- try(workflow.spawn{args} |> map_spawn_error)"
    ));
    Ok(())
}

/// Lower a `wait` statement; with a timeout the binding is optional.
pub(super) fn lower_wait(
    emitter: &mut Emitter<'_>,
    wait: &WaitStmt,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let Some(&signal) = emitter.signals.get(wait.signal.as_str()) else {
        return Err(EmitError::new(
            wait.signal_span,
            format!("`{}` names no declared signal", wait.signal),
        ));
    };
    let payload_ty = type_ref_to_g(&signal.ty);
    let name = ident(&wait.bind.name);
    let receive = format!(
        "workflow.receive({}_signal()) |> map_receive_error",
        snake(&wait.signal)
    );
    match &wait.timeout {
        None => {
            emitter.line(&format!("use {name} <- try({receive})"));
            scope.insert(wait.bind.name.clone(), payload_ty);
        }
        Some(timeout) => {
            let deadline = duration_expr(timeout);
            emitter.line(&format!("use {name} <- try("));
            emitter.indented(|this| {
                this.line(&format!(
                    "case workflow.with_timeout(fn() {{ {receive} }}, {deadline}) {{"
                ));
                this.indented(|this| {
                    this.line("Ok(value) -> Ok(Some(value))");
                    this.line("Error(error.TimedOutError(_)) -> Ok(None)");
                    this.line("Error(error.InnerError(inner)) -> Error(inner)");
                    this.line(
                        "Error(error.TimeoutEngineFailure(message)) -> \
                         Error(AwlTimerFailed(message))",
                    );
                });
                this.line("},");
            });
            emitter.line(")");
            scope.insert(wait.bind.name.clone(), GType::Option(Box::new(payload_ty)));
        }
    }
    Ok(())
}

pub(super) fn lower_sleep(emitter: &mut Emitter<'_>, sleep: &SleepStmt) {
    emitter.line(&format!(
        "use _ <- try(workflow.sleep({}) |> map_timer_error)",
        duration_expr(&sleep.duration)
    ));
}
/// Action/child return-type lookup shared with the binding-type pass.
pub(super) fn action_return(emitter: &Emitter<'_>, name: &str) -> Option<GType> {
    emitter
        .actions
        .get(name)
        .map(|&(_, action)| type_ref_to_g(&action.returns))
        .or_else(|| {
            emitter
                .children
                .get(name)
                .map(|&child| type_ref_to_g(&child.returns))
        })
}
