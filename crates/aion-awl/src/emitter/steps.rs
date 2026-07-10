//! Region, step, substep, and outcome lowering: the control-flow half of
//! the emitter. Every region (dependency-connected step group) becomes one
//! Gleam function; routes are tail calls; conditional outcomes lower to
//! `case` cascades (or a single enum `case` when every arm matches one
//! variant of the same subject); `on failure` wraps the step body in an
//! attempt closure whose error arm runs the compensation.

use crate::ast::{CallStmt, PipeEnd, Statement, Step};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::Scope;
use super::forks::{lower_fork, lower_hetero_parallel};
use super::graph::{Plan, body_ends_in_route, substep_split};
use super::loops::{lower_loop, statement_defs};
use super::names::{ident, snake};
use super::outcomes::{emit_outcomes, emit_route};
use super::pipes::lower_pipe_value;
use super::stmts::{flush_prelude, lower_call, lower_sleep, lower_spawn, lower_wait};

/// Route-resolution frame: `Some` while lowering inside a substep chain.
#[derive(Clone, Copy)]
pub(super) struct Frame<'a> {
    /// Step whose loop functions are being named.
    pub(super) step_name: &'a str,
    /// (parent step index, substep block offset) when inside substeps.
    pub(super) sub: Option<(usize, usize)>,
}

/// Emit `execute`, every region function, and every substep function.
pub(super) fn emit_flow(emitter: &mut Emitter<'_>, plan: &Plan) -> Result<(), EmitError> {
    emit_execute(emitter, plan)?;
    for region_index in 0..plan.regions.len() {
        emit_region(emitter, plan, region_index)?;
    }
    for (position, step) in emitter.document.steps.iter().enumerate() {
        let split = substep_split(step)?;
        if split < step.body.len() {
            super::subs::emit_sub_chain(emitter, plan, position, step, split)?;
        }
    }
    Ok(())
}

fn emit_execute(emitter: &mut Emitter<'_>, plan: &Plan) -> Result<(), EmitError> {
    let output = emitter.output_type();
    let input_type = emitter.input_type.clone();
    emitter.line("/// Workflow body generated from the AWL steps.");
    emitter.line(&format!(
        "pub fn execute(input: {input_type}) -> Result({output}, AwlError) {{"
    ));
    let document = emitter.document;
    let Some(first_region) = plan.regions.iter().position(|region| region.entry == 0) else {
        return Err(EmitError::new(
            document.span,
            "the workflow has no steps to execute",
        ));
    };
    emitter.indented_try(|this| {
        for input in &document.inputs {
            let name = ident(&input.name);
            this.line(&format!("let {name} = input.{name}"));
        }
        let params = plan.region_params(first_region);
        for param in params {
            if !document.inputs.iter().any(|input| &input.name == param) {
                return Err(EmitError::new(
                    document.span,
                    format!(
                        "the workflow start needs `{param}`, which is not an input — the \
                         document did not check cleanly"
                    ),
                ));
            }
        }
        let entry = &document.steps[plan.regions[first_region].entry];
        this.line(&call_region(entry, params));
        Ok(())
    })?;
    emitter.line("}");
    emitter.blank();
    Ok(())
}

fn call_region(entry: &Step, params: &[String]) -> String {
    let args = params
        .iter()
        .map(|name| ident(name))
        .collect::<Vec<_>>()
        .join(", ");
    format!("step_{}({args})", snake(&entry.name))
}

fn emit_region(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
    region_index: usize,
) -> Result<(), EmitError> {
    let region = &plan.regions[region_index];
    let entry = &emitter.document.steps[region.entry];
    let output = emitter.output_type();
    let params = plan.region_params(region_index).to_vec();
    let mut scope = scope_from_params(emitter, &params, entry)?;
    let rendered_params = annotated_params(emitter, &params, &scope);
    emitter.line(&format!(
        "fn step_{}({rendered_params}) -> Result({output}, AwlError) {{",
        snake(&entry.name)
    ));
    let layers = region.layers.clone();
    emitter.indented_try(|this| emit_layers(this, plan, &layers, 0, 0, &mut scope))?;
    emitter.line("}");
    emitter.blank();
    Ok(())
}

/// Render a parameter list with type annotations (Gleam's inference cannot
/// type record access through unannotated parameters).
pub(super) fn annotated_params(emitter: &Emitter<'_>, params: &[String], scope: &Scope) -> String {
    params
        .iter()
        .map(|name| {
            let annotation = scope
                .get(name)
                .map_or_else(|| "Nil".to_owned(), |ty| emitter.env.gleam_type(ty));
            format!("{}: {annotation}", ident(name))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn scope_from_params(
    emitter: &Emitter<'_>,
    params: &[String],
    anchor: &Step,
) -> Result<Scope, EmitError> {
    let mut scope = Scope::new();
    for param in params {
        let Some(ty) = emitter.bindings.get(param) else {
            return Err(EmitError::new(
                anchor.name_span,
                format!(
                    "binding `{param}` flows into step `{}` but its type could not be \
                     established — the document did not check cleanly",
                    anchor.name
                ),
            ));
        };
        scope.insert(param.clone(), ty.clone());
    }
    Ok(scope)
}

/// Emit the region's steps from `(layer, member)` onward, nesting
/// continuations inside `on failure` success arms as needed.
fn emit_layers(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
    layers: &[Vec<usize>],
    layer: usize,
    member: usize,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let Some(current) = layers.get(layer) else {
        return Err(EmitError::new(
            emitter.document.span,
            "a step chain ends without routing — the document did not check cleanly",
        ));
    };
    if member == 0 && current.len() > 1 {
        if let Some(calls) = layer_calls(emitter, current) {
            return emit_parallel_layer(emitter, plan, layers, layer, &calls, scope);
        }
        // Dependency-parallel steps whose bodies are more than one bare
        // action call cannot dispatch concurrently in the Gleam stopgap
        // (the SDK parallelizes activities, not statement sequences) — a
        // recorded mapping limit, and named in the generated module so the
        // degradation is never silent.
        emitter.line("// awl stopgap: these dependency-parallel steps run in written order (the");
        emitter.line("// Gleam SDK has no heterogeneous task primitive for full step bodies).");
    }
    let Some(&step_index) = current.get(member) else {
        return emit_layers(emitter, plan, layers, layer + 1, 0, scope);
    };
    let step = &emitter.document.steps[step_index];
    let next: Continuation<'_> = if member + 1 < current.len() {
        Continuation {
            layers,
            layer,
            member: member + 1,
        }
    } else {
        Continuation {
            layers,
            layer: layer + 1,
            member: 0,
        }
    };
    emit_step(emitter, plan, step_index, step, scope, Some(next))
}

/// Where control goes when a step falls through.
#[derive(Clone, Copy)]
struct Continuation<'a> {
    layers: &'a [Vec<usize>],
    layer: usize,
    member: usize,
}

/// The single bare action call of every member step in a multi-step layer,
/// when the layer is parallelizable: each member must be one call of a
/// declared action with no outcomes or handlers (dependency-parallel steps
/// with fuller bodies fall back to written order — a recorded mapping
/// limit).
fn layer_calls<'a>(emitter: &Emitter<'a>, members: &[usize]) -> Option<Vec<&'a CallStmt>> {
    let mut calls = Vec::new();
    for &member in members {
        let step = &emitter.document.steps[member];
        if !step.outcomes.is_empty() || step.on_failure.is_some() {
            return None;
        }
        let [Statement::Call(call)] = step.body.as_slice() else {
            return None;
        };
        if !emitter.actions.contains_key(call.call.name.as_str()) {
            return None;
        }
        calls.push(call);
    }
    Some(calls)
}

/// One dependency layer of single-call steps as one `workflow.all`: the
/// typed form when every member calls the same action, the raw wire-unified
/// form (see [`lower_hetero_parallel`]) otherwise.
fn emit_parallel_layer(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
    layers: &[Vec<usize>],
    layer: usize,
    calls: &[&CallStmt],
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let homogeneous = calls
        .iter()
        .all(|call| call.call.name == calls[0].call.name);
    if !homogeneous {
        lower_hetero_parallel(emitter, calls, scope)?;
        return emit_layers(emitter, plan, layers, layer + 1, 0, scope);
    }
    let mut values = Vec::new();
    let mut patterns = Vec::new();
    for call in calls {
        let mut prelude = Vec::new();
        let value = super::stmts::activity_value(
            emitter,
            &call.call,
            call.config.as_ref(),
            scope,
            &mut prelude,
        )?;
        flush_prelude(emitter, prelude);
        values.push(value);
        patterns.push(
            call.bind
                .as_ref()
                .map_or_else(|| "_".to_owned(), |bind| ident(&bind.name)),
        );
        if let Some(bind) = &call.bind {
            let (_, action) = emitter.actions[call.call.name.as_str()];
            scope.insert(
                bind.name.clone(),
                super::types::type_ref_to_g(&action.returns),
            );
        }
    }
    emitter.line(&format!(
        "use awl_layer <- try(workflow.all([{}]) |> map_activity_error)",
        values.join(", ")
    ));
    emitter.line(&format!("let assert [{}] = awl_layer", patterns.join(", ")));
    emit_layers(emitter, plan, layers, layer + 1, 0, scope)
}

/// Emit one step: leading statements, substep hand-off or outcomes or
/// fall-through continuation, with `on failure` wrapping when declared.
fn emit_step(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
    step_index: usize,
    step: &Step,
    scope: &mut Scope,
    continuation: Option<Continuation<'_>>,
) -> Result<(), EmitError> {
    let frame = Frame {
        step_name: &step.name,
        sub: None,
    };
    let split = substep_split(step)?;
    let body = &step.body[..split];

    if let Some(on_failure) = &step.on_failure {
        if body_ends_in_route(body) {
            return Err(EmitError::new(
                step.name_span,
                format!(
                    "step `{}` combines `on failure` with a body-terminal route — the Gleam \
                     stopgap cannot tell a routed failure outcome from a step failure there",
                    step.name
                ),
            ));
        }
        let mut defs = std::collections::BTreeSet::new();
        statement_defs(body, &mut defs);
        let defs: Vec<String> = defs.into_iter().collect();
        let mut attempt_scope = scope.clone();
        emitter.line("let awl_attempt = fn() {");
        emitter.indented_try(|this| {
            lower_statements(this, plan, frame, body, &mut attempt_scope, false)?;
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
                emit_step_tail(this, plan, step_index, step, frame, scope, continuation)
            })?;
            this.line("}");
            this.line("Error(_) -> {");
            this.indented_try(|this| {
                let mut compensation_scope = scope.clone();
                lower_statements(
                    this,
                    plan,
                    frame,
                    &on_failure.body,
                    &mut compensation_scope,
                    true,
                )
            })?;
            this.line("}");
            Ok(())
        })?;
        emitter.line("}");
        return Ok(());
    }

    lower_statements(emitter, plan, frame, body, scope, false)?;
    emit_step_tail(emitter, plan, step_index, step, frame, scope, continuation)
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
fn emit_step_tail(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
    step_index: usize,
    step: &Step,
    frame: Frame<'_>,
    scope: &mut Scope,
    continuation: Option<Continuation<'_>>,
) -> Result<(), EmitError> {
    let split = substep_split(step)?;
    if split < step.body.len() {
        let params = plan.sub_params(step_index, 0);
        let Statement::SubStep(first) = &step.body[split] else {
            return Err(EmitError::new(step.name_span, "substep block mis-shaped"));
        };
        let args = params
            .iter()
            .map(|name| ident(name))
            .collect::<Vec<_>>()
            .join(", ");
        emitter.line(&format!(
            "sub_{}_{}({args})",
            snake(&step.name),
            snake(&first.name)
        ));
        return Ok(());
    }
    if !step.outcomes.is_empty() {
        return emit_outcomes(emitter, plan, frame, &step.outcomes, scope);
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
    emit_layers(emitter, plan, next.layers, next.layer, next.member, scope)
}
/// Lower a statement list. Terminal routes render as the tail expression;
/// `expect_route_tail` marks `on failure` bodies, which must end in one.
pub(super) fn lower_statements(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
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
                        lower_statements(this, plan, frame, body, loop_scope, false)
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
                    emit_route(emitter, plan, frame, target, scope, Some(piped))?;
                }
            },
            Statement::Route(route) => {
                if !last {
                    return Err(EmitError::new(
                        route.span,
                        "statements after an unconditional route are unreachable",
                    ));
                }
                emit_route(emitter, plan, frame, &route.target, scope, None)?;
            }
            Statement::SubStep(sub) => {
                return Err(EmitError::new(
                    sub.name_span,
                    "substeps lower only as a step body's trailing block",
                ));
            }
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
