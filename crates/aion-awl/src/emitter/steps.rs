//! Region, step, substep, and outcome lowering: the control-flow half of
//! the emitter, parametric over the flow being lowered (the host workflow,
//! a subflow, or a per-item region member flow — see [`FlowCtx`]). Every
//! region (dependency-connected step group) becomes one Gleam function;
//! routes are tail calls; conditional outcomes lower to `case` cascades (or
//! a single enum `case` when every arm matches one variant of the same
//! subject); `on failure` wraps the step body's fallible prefix in an
//! attempt closure whose error arm runs the compensation — a body-terminal
//! route stays OUTSIDE the closure, so a routed failure outcome
//! (`AwlOutcomeFailure`) can never read as a step failure.

use std::collections::BTreeMap;

use crate::ast::{CallStmt, PipeEnd, PipeStmt, Statement, Step};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, render_expr};
use super::flowshape::{RegionShape, visits_counter};
use super::forks::{lower_fork, lower_hetero_parallel};
use super::graph::{Plan, Plans, body_ends_in_route, substep_split};
use super::loops::{lower_loop, statement_defs};
use super::names::{ident, snake};
use super::outcomes::{emit_outcomes, emit_route};
use super::pipes::lower_pipe_value;
use super::stmts::{flush_prelude, lower_call, lower_sleep, lower_spawn, lower_wait};

/// The flow whose steps are being lowered: the host workflow (`prefix`
/// empty, no exit) or a nested flow (a subflow or a region's per-item
/// member flow), whose functions are name-prefixed and whose exit returns
/// `Ok(...)` instead of a workflow outcome.
pub(super) struct FlowCtx<'f> {
    pub(super) steps: &'f [Step],
    pub(super) regions: &'f BTreeMap<String, RegionShape>,
    pub(super) plan: &'f Plan,
    pub(super) plans: &'f Plans<'f>,
    pub(super) prefix: String,
    pub(super) exit: Option<FlowExit>,
    /// The rendered Gleam type of this flow's `Ok(...)` result.
    pub(super) output: String,
}

/// A nested flow's exit contract.
pub(super) struct FlowExit {
    /// The route-target name that exits the flow.
    pub(super) name: String,
    pub(super) kind: ExitKind,
}

pub(super) enum ExitKind {
    /// A subflow outcome: `route out(<payload>)` returns `Ok(payload)`.
    Subflow { ty: super::types::GType },
    /// A region member flow: reaching (or routing to) the close step
    /// returns `Ok(<collected binding>)`.
    Region { binding: String },
}

impl FlowCtx<'_> {
    pub(super) fn step_fn(&self, name: &str) -> String {
        format!("{}step_{}", self.prefix, snake(name))
    }

    pub(super) fn sub_fn(&self, parent: &str, sub: &str) -> String {
        format!("{}sub_{}_{}", self.prefix, snake(parent), snake(sub))
    }
}

/// Route-resolution frame: `Some` while lowering inside a substep chain.
#[derive(Clone, Copy)]
pub(super) struct Frame<'a> {
    /// Step whose loop functions are being named.
    pub(super) step_name: &'a str,
    /// (parent step index, substep block offset) when inside substeps.
    pub(super) sub: Option<(usize, usize)>,
}

/// Emit `execute`, every host flow function, and every nested flow.
pub(super) fn emit_flow(emitter: &mut Emitter<'_>, plans: &Plans<'_>) -> Result<(), EmitError> {
    let flow = FlowCtx {
        steps: &emitter.document.steps,
        regions: emitter.host_regions,
        plan: &plans.host,
        plans,
        prefix: String::new(),
        exit: None,
        output: emitter.output_type(),
    };
    emit_execute(emitter, &flow, &plans.host_counters)?;
    emit_flow_fns(emitter, &flow)?;
    super::flows::emit_nested(emitter, plans)
}

/// Emit one flow's region functions and substep chains.
pub(super) fn emit_flow_fns(emitter: &mut Emitter<'_>, flow: &FlowCtx<'_>) -> Result<(), EmitError> {
    for region_index in 0..flow.plan.regions.len() {
        emit_region(emitter, flow, region_index)?;
    }
    for (position, step) in flow.steps.iter().enumerate() {
        let split = substep_split(step)?;
        if split < step.body.len() {
            super::subs::emit_sub_chain(emitter, flow, position, step, split)?;
        }
    }
    Ok(())
}

fn emit_execute(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    counters: &[String],
) -> Result<(), EmitError> {
    let output = flow.output.clone();
    let input_type = emitter.input_type.clone();
    emitter.line("/// Workflow body generated from the AWL steps.");
    emitter.line(&format!(
        "pub fn execute(input: {input_type}) -> Result({output}, awl_error.AwlError) {{"
    ));
    let document = emitter.document;
    let Some(first_region) = flow.plan.regions.iter().position(|region| region.entry == 0) else {
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
        // Language-owned visit counters seed once, at the flow's run-once
        // entry, so a backward route can never reset a bound.
        for counter in counters {
            this.line(&format!("let {} = 0", ident(counter)));
        }
        let params = flow.plan.region_params(first_region);
        for param in params {
            let is_input = document.inputs.iter().any(|input| &input.name == param);
            if !is_input && !counters.contains(param) {
                return Err(EmitError::new(
                    document.span,
                    format!(
                        "the workflow start needs `{param}`, which is neither an input nor a \
                         language-owned counter — the document did not check cleanly"
                    ),
                ));
            }
        }
        let entry = &flow.steps[flow.plan.regions[first_region].entry];
        let args = params
            .iter()
            .map(|name| ident(name))
            .collect::<Vec<_>>()
            .join(", ");
        this.line(&format!("{}({args})", flow.step_fn(&entry.name)));
        Ok(())
    })?;
    emitter.line("}");
    emitter.blank();
    Ok(())
}

fn emit_region(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    region_index: usize,
) -> Result<(), EmitError> {
    let region = &flow.plan.regions[region_index];
    let entry = &flow.steps[region.entry];
    let output = flow.output.clone();
    let params = flow.plan.region_params(region_index).to_vec();
    let mut scope = scope_from_params(emitter, &params, entry)?;
    let rendered_params = annotated_params(emitter, &params, &scope);
    emitter.line(&format!(
        "fn {}({rendered_params}) -> Result({output}, awl_error.AwlError) {{",
        flow.step_fn(&entry.name)
    ));
    let layers = region.layers.clone();
    let region_last = layers.iter().flatten().copied().max().unwrap_or(region.entry);
    emitter.indented_try(|this| emit_layers(this, flow, &layers, 0, 0, region_last, &mut scope))?;
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
    flow: &FlowCtx<'_>,
    layers: &[Vec<usize>],
    layer: usize,
    member: usize,
    region_last: usize,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let Some(current) = layers.get(layer) else {
        return emit_flow_end(emitter, flow, region_last, scope);
    };
    if member == 0 && current.len() > 1 {
        if let Some(calls) = layer_calls(emitter, flow, current) {
            return emit_parallel_layer(emitter, flow, layers, layer, region_last, &calls, scope);
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
        return emit_layers(emitter, flow, layers, layer + 1, 0, region_last, scope);
    };
    let step = &flow.steps[step_index];
    let next: Continuation<'_> = if member + 1 < current.len() {
        Continuation {
            layers,
            layer,
            member: member + 1,
            region_last,
        }
    } else {
        Continuation {
            layers,
            layer: layer + 1,
            member: 0,
            region_last,
        }
    };
    emit_step(emitter, flow, step_index, step, scope, Some(next))
}

/// Where control goes when the flow's last region layer completes: an
/// implicit tail call into the next step's region, a nested flow's exit
/// return, or the honest refusal.
fn emit_flow_end(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    region_last: usize,
    scope: &Scope,
) -> Result<(), EmitError> {
    let next = region_last + 1;
    if next < flow.steps.len() {
        let target = &flow.steps[next];
        let Some(region) = flow.plan.region_of_entry(next) else {
            return Err(EmitError::new(
                target.name_span,
                format!(
                    "control falls into `{}`, which does not head a region — the Gleam \
                     stopgap cannot express that hand-off",
                    target.name
                ),
            ));
        };
        let args = flow
            .plan
            .region_params(region)
            .iter()
            .map(|name| ident(name))
            .collect::<Vec<_>>()
            .join(", ");
        emitter.line(&format!("{}({args})", flow.step_fn(&target.name)));
        return Ok(());
    }
    match &flow.exit {
        Some(FlowExit {
            kind: ExitKind::Region { binding },
            ..
        }) => {
            let _ = scope;
            emitter.line(&format!("Ok({})", ident(binding)));
            Ok(())
        }
        Some(FlowExit {
            kind: ExitKind::Subflow { .. },
            name,
        }) => Err(EmitError::new(
            emitter.document.span,
            format!(
                "a subflow's last step must route to its outcome `{name}` — the document \
                 did not check cleanly"
            ),
        )),
        None => Err(EmitError::new(
            emitter.document.span,
            "a step chain ends without routing — the document did not check cleanly",
        )),
    }
}

/// Where control goes when a step falls through.
#[derive(Clone, Copy)]
pub(super) struct Continuation<'a> {
    layers: &'a [Vec<usize>],
    layer: usize,
    member: usize,
    region_last: usize,
}

/// The single bare action call of every member step in a multi-step layer,
/// when the layer is parallelizable: each member must be one call of a
/// declared action with no outcomes or handlers (dependency-parallel steps
/// with fuller bodies fall back to written order — a recorded mapping
/// limit).
fn layer_calls<'a>(
    emitter: &Emitter<'_>,
    flow: &FlowCtx<'a>,
    members: &[usize],
) -> Option<Vec<&'a CallStmt>> {
    let mut calls = Vec::new();
    for &member in members {
        let step = &flow.steps[member];
        if !step.outcomes.is_empty() || step.on_failure.is_some() || step.max_visits.is_some() {
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
    flow: &FlowCtx<'_>,
    layers: &[Vec<usize>],
    layer: usize,
    region_last: usize,
    calls: &[&CallStmt],
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let homogeneous = calls
        .iter()
        .all(|call| call.call.name == calls[0].call.name);
    if !homogeneous {
        lower_hetero_parallel(emitter, calls, scope)?;
        return emit_layers(emitter, flow, layers, layer + 1, 0, region_last, scope);
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
        "use awl_layer <- result.try(workflow.all([{}]) |> awl_error.map_activity_error)",
        values.join(", ")
    ));
    emitter.line(&format!("let assert [{}] = awl_layer", patterns.join(", ")));
    emit_layers(emitter, flow, layers, layer + 1, 0, region_last, scope)
}

/// Emit one step: the visit-bound prologue when declared, leading
/// statements, substep hand-off or outcomes or fall-through continuation,
/// with `on failure` wrapping when declared.
fn emit_step(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    step_index: usize,
    step: &Step,
    scope: &mut Scope,
    continuation: Option<Continuation<'_>>,
) -> Result<(), EmitError> {
    let frame = Frame {
        step_name: &step.name,
        sub: None,
    };
    if let Some(max_visits) = &step.max_visits {
        emit_visits_prologue(emitter, step, max_visits, scope)?;
    }
    let split = substep_split(step)?;
    let body = &step.body[..split];

    if let Some(on_failure) = &step.on_failure {
        let on_failure_body = on_failure.body.clone();
        return emit_with_failure(
            emitter,
            flow,
            frame,
            body,
            &on_failure_body,
            scope,
            &mut |this, scope| emit_step_tail(this, flow, step_index, step, frame, scope, continuation),
        );
    }

    lower_statements(emitter, flow, frame, body, scope, false)?;
    emit_step_tail(emitter, flow, step_index, step, frame, scope, continuation)
}

/// The visit-bound prologue of a `max … visits` step: increment the
/// language-owned counter and refuse the visit past the bound with the
/// spanned `AwlVisitsExceeded` runtime failure.
fn emit_visits_prologue(
    emitter: &mut Emitter<'_>,
    step: &Step,
    max_visits: &crate::ast::MaxVisits,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let counter = ident(&visits_counter(&step.name));
    let mut prelude = Vec::new();
    let bound = render_expr(emitter, &max_visits.bound, scope, &mut prelude)?;
    if !prelude.is_empty() {
        return Err(EmitError::new(
            max_visits.span,
            "indexing inside a `max … visits` bound is not lowerable in the Gleam stopgap",
        ));
    }
    emitter.line(&format!("let {counter} = {counter} + 1"));
    let message = format!(
        "step `{}` exceeded its `max … visits` bound at line {}, column {}",
        step.name, max_visits.span.line, max_visits.span.column
    );
    emitter.line(&format!("use _ <- result.try(case {counter} > {bound} {{"));
    emitter.indented(|this| {
        this.line(&format!(
            "True -> Error(awl_error.AwlVisitsExceeded({}))",
            super::names::string_lit(&message)
        ));
        this.line("False -> Ok(Nil)");
    });
    emitter.line("})");
    scope.insert(visits_counter(&step.name), super::types::GType::Int);
    Ok(())
}

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
fn emit_step_tail(
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
