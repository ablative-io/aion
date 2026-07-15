//! Failure-boundary and statement-list lowering shared by steps and substeps.
//! A routed workflow outcome remains outside the fallible attempt closure, so
//! `AwlOutcomeFailure` is never mistaken for an operation failure and never
//! triggers compensation.

use crate::ast::{PipeEnd, PipeStmt, Statement, Step};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::Scope;
use super::forks::lower_fork;
use super::graph::{body_ends_in_route, substep_split};
use super::loops::{lower_loop, statement_defs};
use super::names::ident;
use super::outcomes::{emit_outcomes, emit_route};
use super::pipes::lower_pipe_value;
use super::steps::{Continuation, FlowCtx, Frame, emit_layers};
use super::stmts::{lower_call, lower_sleep, lower_spawn, lower_wait};

/// The body statements covered by an `on failure` attempt closure, and the
/// terminal route (when the body ends in one) that stays OUTSIDE it.
enum TailRoute<'b> {
    None,
    Route(&'b crate::ast::RouteTarget),
    Pipe(&'b PipeStmt),
}

/// Lower a step or substep body under `on failure`: the fallible prefix
/// runs in an attempt closure; a body-terminal route (including a piped
/// route, whose VALUE computation is fallible and stays inside) renders in
/// the success arm as a genuine tail — so `Error(AwlOutcomeFailure(…))`
/// from the route is never mistaken for a step failure, and compensation
/// runs only on captured operation failures.
pub(super) fn emit_with_failure(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    frame: Frame<'_>,
    body: &[Statement],
    on_failure: &[Statement],
    scope: &mut Scope,
    success_tail: &mut dyn FnMut(&mut Emitter<'_>, &mut Scope) -> Result<(), EmitError>,
) -> Result<(), EmitError> {
    let (attempt_body, tail) = match body.last() {
        Some(Statement::Route(route)) => (&body[..body.len() - 1], TailRoute::Route(&route.target)),
        Some(Statement::Pipe(pipe)) if matches!(pipe.end, PipeEnd::Route(_)) => {
            (&body[..body.len() - 1], TailRoute::Pipe(pipe))
        }
        _ => (body, TailRoute::None),
    };
    let mut defs = std::collections::BTreeSet::new();
    statement_defs(attempt_body, &mut defs);
    let mut defs: Vec<String> = defs.into_iter().collect();
    let carrier = "awl_route_payload";
    let mut piped_ty = None;
    let mut attempt_scope = scope.clone();
    emitter.line("let awl_attempt = fn() {");
    emitter.indented_try(|this| {
        lower_statements(this, flow, frame, attempt_body, &mut attempt_scope, false)?;
        if let TailRoute::Pipe(pipe) = &tail {
            let (value, ty) = lower_pipe_value(this, pipe, &attempt_scope)?;
            this.line(&format!("let {carrier} = {value}"));
            attempt_scope.insert(carrier.to_owned(), ty.clone());
            piped_ty = Some(ty);
            defs.push(carrier.to_owned());
            defs.sort();
        }
        let tuple = render_defs_tuple(&defs);
        this.line(&format!("Ok({tuple})"));
        Ok(())
    })?;
    emitter.line("}");
    let pattern = render_defs_tuple(&defs);
    emitter.line("case awl_attempt() {");
    emitter.indented_try(|this| {
        this.line(&format!("Ok({pattern}) -> {{"));
        this.indented_try(|this| {
            for name in &defs {
                if let Some(ty) = attempt_scope.get(name) {
                    scope.insert(name.clone(), ty.clone());
                }
            }
            match &tail {
                TailRoute::None => success_tail(this, scope),
                TailRoute::Route(target) => emit_route(this, flow, frame, target, scope, None),
                TailRoute::Pipe(pipe) => {
                    let PipeEnd::Route(target) = &pipe.end else {
                        return Err(EmitError::new(pipe.span, "piped route lost its target"));
                    };
                    let Some(ty) = piped_ty.clone() else {
                        return Err(EmitError::new(pipe.span, "piped route lost its value type"));
                    };
                    emit_route(
                        this,
                        flow,
                        frame,
                        target,
                        scope,
                        Some((carrier.to_owned(), ty)),
                    )
                }
            }
        })?;
        this.line("}");
        this.line("Error(_) -> {");
        this.indented_try(|this| {
            let mut compensation_scope = scope.clone();
            lower_statements(this, flow, frame, on_failure, &mut compensation_scope, true)
        })?;
        this.line("}");
        Ok(())
    })?;
    emitter.line("}");
    Ok(())
}

pub(super) fn render_defs_tuple(defs: &[String]) -> String {
    match defs {
        [] => "Nil".to_owned(),
        [single] => ident(single),
        many => format!(
            "#({})",
            many.iter()
                .map(|name| ident(name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// After a step's leading statements: substep hand-off, outcome clauses,
/// an already-emitted terminal route, or the fall-through continuation.
pub(super) fn emit_step_tail(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    step_index: usize,
    step: &Step,
    frame: Frame<'_>,
    scope: &mut Scope,
    continuation: Option<Continuation<'_>>,
) -> Result<(), EmitError> {
    let split = substep_split(step)?;
    if split < step.body.len() {
        let params = flow.plan.sub_params(step_index, 0);
        let Statement::SubStep(first) = &step.body[split] else {
            return Err(EmitError::new(step.name_span, "substep block mis-shaped"));
        };
        let args = params
            .iter()
            .map(|name| ident(name))
            .collect::<Vec<_>>()
            .join(", ");
        emitter.line(&format!("{}({args})", flow.sub_fn(&step.name, &first.name)));
        return Ok(());
    }
    if !step.outcomes.is_empty() {
        return emit_outcomes(emitter, flow, frame, &step.outcomes, scope);
    }
    if body_ends_in_route(&step.body) {
        // The route rendered as the body's tail expression already.
        return Ok(());
    }
    let Some(next) = continuation else {
        return Err(EmitError::new(
            step.name_span,
            format!(
                "step `{}` completes with nowhere to go — the document did not check cleanly",
                step.name
            ),
        ));
    };
    emit_layers(
        emitter,
        flow,
        next.layers,
        next.layer,
        next.member,
        next.region_last,
        scope,
    )
}
/// Lower a statement list. Terminal routes render as the tail expression;
/// `expect_route_tail` marks `on failure` bodies, which must end in one.
pub(super) fn lower_statements(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    frame: Frame<'_>,
    statements: &[Statement],
    scope: &mut Scope,
    expect_route_tail: bool,
) -> Result<(), EmitError> {
    for (position, statement) in statements.iter().enumerate() {
        let last = position + 1 == statements.len();
        match statement {
            Statement::Call(call) => lower_call(emitter, call, scope)?,
            Statement::Spawn(spawn) => lower_spawn(emitter, spawn, scope)?,
            Statement::Wait(wait) => lower_wait(emitter, wait, scope)?,
            Statement::Sleep(sleep) => lower_sleep(emitter, sleep),
            Statement::Fork(fork) => lower_fork(emitter, fork, scope)?,
            Statement::Loop(looped) => {
                let step_name = frame.step_name.to_owned();
                lower_loop(
                    emitter,
                    &step_name,
                    looped,
                    scope,
                    &mut |this, body, loop_scope| {
                        lower_statements(this, flow, frame, body, loop_scope, false)
                    },
                )?;
            }
            Statement::Pipe(pipe) => match &pipe.end {
                PipeEnd::Bind(binding) => {
                    let (value, ty) = lower_pipe_value(emitter, pipe, scope)?;
                    emitter.line(&format!("let {} = {value}", ident(&binding.name)));
                    scope.insert(binding.name.clone(), ty);
                }
                PipeEnd::Route(target) => {
                    if !last {
                        return Err(EmitError::new(
                            pipe.span,
                            "statements after an unconditional route are unreachable",
                        ));
                    }
                    let piped = lower_pipe_value(emitter, pipe, scope)?;
                    emit_route(emitter, flow, frame, target, scope, Some(piped))?;
                }
            },
            Statement::Route(route) => {
                if !last {
                    return Err(EmitError::new(
                        route.span,
                        "statements after an unconditional route are unreachable",
                    ));
                }
                emit_route(emitter, flow, frame, &route.target, scope, None)?;
            }
            Statement::SubStep(sub) => {
                return Err(EmitError::new(
                    sub.name_span,
                    "substeps lower only as a step body's trailing block",
                ));
            }
            // The collapsed region step's fan-out pair: the header lowers
            // the whole delivery + collect; the collect marker is consumed.
            Statement::Distribute(distribute) => {
                super::flows::emit_fanout(emitter, flow, frame.step_name, distribute, scope)?;
            }
            Statement::Collect(_) => {}
        }
    }
    if expect_route_tail
        && !matches!(
            statements.last(),
            Some(
                Statement::Route(_)
                    | Statement::Pipe(crate::ast::PipeStmt {
                        end: PipeEnd::Route(_),
                        ..
                    })
            )
        )
    {
        return Err(EmitError::new(
            emitter.document.span,
            "an `on failure` block must end in a route",
        ));
    }
    Ok(())
}
