//! Region-body lowering for the BC-2 covered subset: sequential regions whose
//! step bodies are action calls (with per-key call-site config merge), awaited
//! child calls and fire-and-forget spawns (`child_call`), waits with and
//! without timeouts (`wait`), sleeps, and pipe chains (action/child/field/
//! combinator stages — `pipes`) ending in a route. A multi-step chain lowers
//! as one `FlowFn` per step, each non-terminal step ending in a tail call to
//! its successor (IR-14) with the chain-boundary live set
//! (`chain::chain_params`) as arguments. Bounded loops lower through `loops`
//! (a self-tail-calling `FlowFn(Loop)` per loop). Forks lower through `forks`
//! (activity fan-out over `workflow.map`/`all` / `list.try_fold` and child
//! spawn-all/ordered-await fan-out to the reference emitter's parity
//! contract).
//!
//! Still deferred (`LowerError::unsupported`, visible incompleteness — never
//! silent divergence from the reference):
//! - substeps: mechanical but large (per-substep fns over the shared plan's
//!   `sub_params`/`sub_node`, sibling/parent route frames, `on failure`
//!   interaction — reference `emitter/subs.rs:20-60`, shape gates
//!   `emitter/graph.rs:73-116`) against one valid-fixture pressure point
//!   (`loop-outcomes/valid/substeps_two_stage.awl`) — `FnOrigin::SubStep`
//!   already exists;
//! - `on failure`: needs the reserved `Stmt::Attempt` select emission —
//!   instruction selection refuses it (`select/flow.rs::unsupported_stmt`),
//!   so support is select-surface work (attempt closure call + defs-tuple
//!   threading mirroring `emitter/steps.rs:301-354`), beyond lowering scope;
//!   the lowering sketch: lift the attempt body to a `FlowFn` returning
//!   `Ok(defs-tuple)`, host does `CallLocal` + `Tail::If IsTagged(ok, arity
//!   2)` with the compensation block (which must end in a route,
//!   `emitter/steps.rs:485-501`) in the else arm — reconvergence is
//!   impossible, so the success continuation nests in the then-arm; refuse
//!   the body-terminal-route combination with the emitter's class
//!   (`emitter/steps.rs:302-311`). Fixture pressure:
//!   `on_failure_compensation.awl`, `ship_release_combined.awl`;
//! - dependency-parallel region layers (`parallel region`). The reference is
//!   richer twice over (`emitter/steps.rs`): single-bare-call layers (every
//!   member = one unbound declared-action call, no outcomes/on-failure —
//!   `layer_calls`) dispatch as ONE `workflow.all`, typed when homogeneous,
//!   raw twins + per-position decode when heterogeneous
//!   (`lower_hetero_parallel`); richer layers degrade to WRITTEN ORDER with
//!   a generated visibility comment. Design if implemented: flatten
//!   `region.layers` into the emitter's step walk; for a multi-member layer
//!   first try the single-call-layer gate, lowering the typed/raw
//!   `workflow.all` by reusing `fork_named`'s machinery (`AssertList` binds
//!   + `bind_branches` per-position decode); otherwise lower members
//!   sequentially and set `degraded_parallel: true` on the region `FlowFn`
//!   (the MIR twin of the emitter's comment, printed as S13). Planning
//!   impact: `chain_params`/`plan.chains` assume one member per layer — the
//!   flattened order must be identical at plan and lower time; hetero layers
//!   need raw twins planned, so `forks::raw_action_inventory` must also scan
//!   multi-member layers. Fixtures unlocked:
//!   `dag-fork/valid/after_multi_diamond.awl` (all-layer),
//!   `dag-fork/valid/release_pipeline_combined.awl` +
//!   `flagship/valid/dev_brief.awl` (degraded).
//!
//! Parity refusals (the reference emitter refuses these too — a language
//! boundary, not a direct-path gap): mid-chain routes (the shared `Plan`
//! refuses step-level routes to non-entry steps, `emitter/graph.rs:331-345` /
//! `emitter/outcomes.rs:91-95` — `route_tail`'s `region_of_entry` miss),
//! indexing inside outcome guards (`outcome::lower_outcomes`), and the fork
//! boundaries named in `forks`. The activity-emission primitives live in
//! `activity`.

use crate::RouteDirection;
use crate::ast::{CallStmt, PipeEnd, RouteTarget, Statement, Step};
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
use super::expr::{Binding, Scope, lower_arg_for};
use super::forks::lower_fork_stmt;
use super::loops::lower_loop_stmt;
use super::outcome::lower_outcomes;
use super::slots::Slots;

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
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let mut retired_outcome_anchor = None;
    for region_index in 0..ctx.plan.regions.len() {
        let outcome_anchor = ctx.plan.regions[region_index]
            .layers
            .iter()
            .flatten()
            .find_map(|step_index| {
                let step = &ctx.emitter.document.steps[*step_index];
                (!step.outcomes.is_empty()).then_some(step.name_span)
            });
        if let Err(error) = lower_region(ctx, plan, region_index, functions, slots) {
            return Err(match retired_outcome_anchor {
                Some(anchor) => error.reanchor_unsupported(anchor),
                None => error,
            });
        }
        retired_outcome_anchor = retired_outcome_anchor.or(outcome_anchor);
    }
    Ok(())
}

fn lower_region(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    region_index: usize,
    functions: &mut Vec<MirFn>,
    slots: &mut Slots,
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
        let flow = lower_chain_step(
            ctx,
            ChainStep {
                plan,
                entry_step: &entry_step,
                step: &step,
                position,
                param_names: &param_names,
                next,
            },
            slots,
        )?;
        functions.push(MirFn::Flow(flow));
    }
    Ok(())
}

struct ChainStep<'a> {
    plan: &'a FnPlan,
    entry_step: &'a Step,
    step: &'a Step,
    position: usize,
    param_names: &'a [String],
    next: Option<Next>,
}

fn lower_chain_step(
    ctx: &mut Ctx<'_>,
    chain: ChainStep<'_>,
    slots: &mut Slots,
) -> Result<FlowFn, LowerError> {
    ctx.reset_vars();
    let mut scope: Scope = Scope::new();
    let mut param_vars = Vec::new();
    let mut param_tys = Vec::new();
    for name in chain.param_names {
        let ty = ctx.emitter.bindings.get(name).cloned().ok_or_else(|| {
            LowerError::new(
                chain.step.name_span,
                format!("binding `{name}` has no type"),
            )
        })?;
        let var = ctx.fresh_var();
        param_vars.push(var);
        param_tys.push(ctx.tydesc(&ty));
        scope.insert(name.clone(), Binding { var, ty });
    }

    let body = lower_step(ctx, chain.plan, chain.step, &mut scope, chain.next, slots)?;
    let origin = if chain.position == 0 {
        FnOrigin::Region {
            entry_step: chain.entry_step.name.clone(),
        }
    } else {
        FnOrigin::ChainStep {
            entry_step: chain.entry_step.name.clone(),
            step: chain.step.name.clone(),
        }
    };
    Ok(FlowFn {
        origin,
        name: format!("step_{}", snake(&chain.step.name)),
        params: param_vars,
        param_tys,
        ret_ty: TyDesc::Result(Box::new(output_tydesc(ctx)), Box::new(TyDesc::AwlError)),
        body,
        span: Span::from_source(chain.step.name_span),
        degraded_parallel: false,
    })
}

fn lower_step(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    scope: &mut Scope,
    next: Option<Next>,
    slots: &mut Slots,
) -> Result<Block, LowerError> {
    if step.on_failure.is_some() {
        return Err(LowerError::unsupported("on failure", step.name_span));
    }
    if step.body.iter().any(|s| matches!(s, Statement::SubStep(_))) {
        return Err(LowerError::unsupported("substeps", step.name_span));
    }
    let mut stmts = Vec::new();
    for statement in &step.body {
        if let Some(tail) = lower_statement(ctx, plan, step, statement, scope, &mut stmts, slots)? {
            return Ok(Block { stmts, tail });
        }
    }
    if !step.outcomes.is_empty() {
        let outcome = lower_outcomes(ctx, plan, &step.outcomes, scope)
            .map_err(|error| error.reanchor_unsupported(step.name_span))?;
        stmts.extend(outcome.stmts);
        return Ok(Block {
            stmts,
            tail: outcome.tail,
        });
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

pub(super) fn lower_statement(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    statement: &Statement,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
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
            let (value, ty) =
                super::pipes::lower_pipe_value(ctx, plan, &pipe.head, &pipe.stages, scope, stmts)?;
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
        Statement::Spawn(spawn) => {
            super::child_call::lower_spawn_stmt(ctx, plan, spawn, scope, stmts)?;
            Ok(None)
        }
        Statement::Wait(wait) => {
            super::wait::lower_wait_stmt(ctx, plan, step, wait, scope, stmts, slots)?;
            Ok(None)
        }
        Statement::Fork(fork) => {
            lower_fork_stmt(ctx, plan, step, fork, scope, stmts, slots)?;
            Ok(None)
        }
        Statement::Loop(looped) => {
            lower_loop_stmt(ctx, plan, step, looped, scope, stmts, slots)?;
            Ok(None)
        }
        Statement::SubStep(sub) => Err(LowerError::unsupported("substep", sub.name_span)),
    }
}

pub(super) fn lower_call(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    call: &CallStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(), LowerError> {
    // An awaited child call dispatches through the string-name spawn form;
    // config on a child call refuses with the reference's class
    // (`child_call::lower_child_call_stmt`).
    if ctx.emitter.children.contains_key(call.call.name.as_str()) {
        return super::child_call::lower_child_call_stmt(ctx, plan, call, scope, stmts);
    }
    let bound = activity_call(
        ctx,
        plan,
        &call.call,
        call.config.as_ref(),
        None,
        scope,
        stmts,
    )?;
    if let Some(bind) = &call.bind {
        let ty = ctx
            .emitter
            .actions
            .get(call.call.name.as_str())
            .map(|&(_, decl)| type_ref_to_g(&decl.returns))
            .ok_or_else(|| {
                LowerError::new(
                    call.call.name_span,
                    format!(
                        "`{}` names neither a declared action nor a child workflow",
                        call.call.name
                    ),
                )
            })?;
        scope.insert(bind.name.clone(), Binding { var: bound, ty });
    }
    Ok(())
}

pub(super) fn route_tail(
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
                let json = encode_json(ctx, plan, &info.ty, payload, stmts)?;
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
