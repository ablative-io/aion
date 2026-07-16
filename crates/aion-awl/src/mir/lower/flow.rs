//! Region-body lowering, parametric over the flow being lowered (the host
//! workflow or a per-item region member flow — [`FlowCtx`], the MIR twin of
//! the reference `emitter/steps.rs::FlowCtx`). A multi-step chain lowers as
//! one `FlowFn` per step, each non-terminal step ending in a tail call to
//! its successor (IR-14) with the chain-boundary live set
//! (`chain::chain_params`) as arguments; a `max … visits` step opens with the
//! increment-and-test prologue; a chain whose last step falls through hands
//! control to the next step's region (or a member flow's exit return of the
//! collected binding). Bounded loops lower through `loops`, forks through
//! `forks`, collapsed per-item regions through `fanout`, routes through
//! `route`.
//!
//! Still deferred (`LowerError::unsupported`, visible incompleteness — never
//! silent divergence from the reference):
//! - substeps: mechanical but large (per-substep fns over the shared plan's
//!   `sub_params`/`sub_node`, sibling/parent route frames, `on failure`
//!   interaction — reference `emitter/subs.rs:20-60`) — `FnOrigin::SubStep`
//!   already exists;
//! - `on failure`: needs the reserved `Stmt::Attempt` select emission —
//!   instruction selection refuses it (`select/flow.rs::unsupported_stmt`),
//!   so support is select-surface work beyond lowering scope;
//! - `subflow` declarations and value route payloads (`route out(<value>)`):
//!   refused at the driver's staging gate;
//! - dependency-parallel region layers (`parallel region`) — see the design
//!   sketch retained in the module history (flatten `region.layers` into the
//!   emitter's step walk, `workflow.all` single-call layers, degrade richer
//!   layers to written order with the S13 marker).
//!
//! Parity refusals (the reference emitter refuses these too — a language
//! boundary, not a direct-path gap): mid-chain routes, indexing inside
//! outcome guards and `max … visits` bounds, and the fork/fan-out boundaries
//! named in `forks`/`fanout`.

use std::collections::BTreeMap;

use crate::ast::{CallStmt, PipeEnd, Statement, Step};
use crate::emitter::{GType, Plan, RegionShape, snake, type_ref_to_g};

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{Block, Stmt, Tail, Value};
use super::super::tydesc::TyDesc;
use super::activity::{activity_call, lower_sleep, record_new, zero_span};
use super::build::{FlowFns, FnPlan};
use super::chain::chain_params;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope};
use super::forks::lower_fork_stmt;
use super::loops::lower_loop_stmt;
use super::outcome::lower_outcomes;
use super::route::route_tail;
use super::slots::Slots;

/// A member flow's exit contract: routing to (or falling into) the close
/// step returns `Ok(<collected binding>)`.
pub(super) struct FlowExit {
    /// The route-target name that exits the flow (the close step's name).
    pub(super) name: String,
    /// The per-instance binding the collect gathers.
    pub(super) binding: String,
}

/// The flow whose steps are being lowered: the host workflow (`prefix`
/// empty, no exit) or a per-item region member flow, whose functions are
/// name-prefixed and whose exit returns `Ok(<collected binding>)` instead of
/// a workflow outcome.
pub(super) struct FlowCtx<'a> {
    pub(super) steps: &'a [Step],
    pub(super) regions: &'a BTreeMap<String, RegionShape>,
    pub(super) plan: &'a Plan,
    /// This flow's reserved region-chain function refs.
    pub(super) fns: &'a FlowFns,
    /// Binding/type environment owned by this flow.
    pub(super) bindings: &'a BTreeMap<String, GType>,
    pub(super) exit: Option<FlowExit>,
    /// Generated-name prefix (`""` for the host, `"awl_r<id>_<open>_"`).
    pub(super) prefix: String,
    /// Origin qualifier for nested flows (`r0:wave`).
    pub(super) label: Option<String>,
    /// The full return type of this flow's functions (`Result(_, AwlError)`).
    pub(super) ret_ty: TyDesc,
}

impl FlowCtx<'_> {
    fn origin_step(&self, step: &str) -> String {
        match &self.label {
            Some(label) => format!("{label}::{step}"),
            None => step.to_owned(),
        }
    }
}

/// The per-flow lowering environment: the module-wide fn-ref plan plus the
/// flow being lowered.
#[derive(Clone, Copy)]
pub(super) struct FlowEnv<'a> {
    pub(super) plan: &'a FnPlan,
    pub(super) flow: &'a FlowCtx<'a>,
}

/// The fall-through continuation of a non-terminal chain step: the successor's
/// function ref and parameter names.
struct Next {
    callee: FnRef,
    param_names: Vec<String>,
}

/// Lower every region of one flow into `step_<name>` `FlowFn`s (one per chain
/// step, in chain order), appended to `functions`.
pub(super) fn lower_flow_fns(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    functions: &mut Vec<MirFn>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let mut retired_outcome_anchor = None;
    for region_index in 0..env.flow.plan.regions.len() {
        let outcome_anchor = env.flow.plan.regions[region_index]
            .layers
            .iter()
            .flatten()
            .find_map(|step_index| {
                let step = &env.flow.steps[*step_index];
                (!step.outcomes.is_empty()).then_some(step.name_span)
            });
        if let Err(error) = lower_region(ctx, env, region_index, functions, slots) {
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
    env: FlowEnv<'_>,
    region_index: usize,
    functions: &mut Vec<MirFn>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let flow = env.flow;
    let region = &flow.plan.regions[region_index];
    let entry_index = region.entry;
    let layers = region.layers.clone();
    let entry_step = flow.steps[entry_index].clone();

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
    let region_last = chain.last().copied().unwrap_or(entry_index);

    let params = chain_params(ctx, flow, &chain)?;
    for (position, &step_index) in chain.iter().enumerate() {
        let step = flow.steps[step_index].clone();
        // The entry's parameter list is the shared plan's fixed point (the
        // parity anchor); chain boundaries use the backward live sets.
        let param_names = if position == 0 {
            flow.plan.region_params(region_index).to_vec()
        } else {
            params[position].clone()
        };
        let next = chain.get(position + 1).map(|_| Next {
            callee: flow.fns.chains[region_index][position + 1],
            param_names: params[position + 1].clone(),
        });
        let chain_step = ChainStep {
            entry_step: &entry_step,
            step: &step,
            position,
            param_names: &param_names,
            next,
            region_last,
        };
        let flow_fn = lower_chain_step(ctx, env, chain_step, slots)?;
        functions.push(MirFn::Flow(flow_fn));
    }
    Ok(())
}

struct ChainStep<'a> {
    entry_step: &'a Step,
    step: &'a Step,
    position: usize,
    param_names: &'a [String],
    next: Option<Next>,
    region_last: usize,
}

fn lower_chain_step(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    chain: ChainStep<'_>,
    slots: &mut Slots,
) -> Result<FlowFn, LowerError> {
    let flow = env.flow;
    ctx.reset_vars();
    let mut scope: Scope = Scope::new();
    let mut param_vars = Vec::new();
    let mut param_tys = Vec::new();
    for name in chain.param_names {
        let ty = flow.bindings.get(name).cloned().ok_or_else(|| {
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

    // The visit-bound prologue of a `max … visits` step: increment the
    // language-owned counter and refuse the visit past the bound with the
    // spanned `AwlVisitsExceeded` runtime failure (the reference
    // `emitter/steps.rs::emit_visits_prologue`).
    let mut prologue = Vec::new();
    let guard = match &chain.step.max_visits {
        Some(max_visits) => Some(super::visits::visits_prologue(
            ctx,
            chain.step,
            max_visits,
            &mut scope,
            &mut prologue,
        )?),
        None => None,
    };
    let body = lower_step(
        ctx,
        env,
        chain.step,
        &mut scope,
        chain.next,
        chain.region_last,
        slots,
    )?;
    let body = match guard {
        Some((test, error_block)) => Block {
            stmts: prologue,
            tail: Tail::If {
                test,
                then_block: error_block,
                else_block: Box::new(body),
                span: Span::from_source(chain.step.name_span),
            },
        },
        None => body,
    };
    let origin = if chain.position == 0 {
        FnOrigin::Region {
            entry_step: flow.origin_step(&chain.entry_step.name),
        }
    } else {
        FnOrigin::ChainStep {
            entry_step: flow.origin_step(&chain.entry_step.name),
            step: chain.step.name.clone(),
        }
    };
    Ok(FlowFn {
        origin,
        name: format!("{}step_{}", flow.prefix, snake(&chain.step.name)),
        params: param_vars,
        param_tys,
        ret_ty: flow.ret_ty.clone(),
        body,
        span: Span::from_source(chain.step.name_span),
        degraded_parallel: false,
    })
}

fn lower_step(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    step: &Step,
    scope: &mut Scope,
    next: Option<Next>,
    region_last: usize,
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
        if let Some(tail) = lower_statement(ctx, env, step, statement, scope, &mut stmts, slots)? {
            return Ok(Block { stmts, tail });
        }
    }
    if !step.outcomes.is_empty() {
        let outcome = lower_outcomes(ctx, env, &step.outcomes, scope)
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
    flow_end(ctx, env, step, scope, region_last, stmts)
}

/// Where control goes when the flow's last chain step completes: an implicit
/// tail call into the next step's region, a member flow's exit return, or the
/// honest refusal (the reference `emitter/steps.rs::emit_flow_end`).
fn flow_end(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    step: &Step,
    scope: &Scope,
    region_last: usize,
    mut stmts: Vec<Stmt>,
) -> Result<Block, LowerError> {
    let flow = env.flow;
    let next = region_last + 1;
    if next < flow.steps.len() {
        let target = &flow.steps[next];
        let region = flow.plan.region_of_entry(next).ok_or_else(|| {
            LowerError::new(
                target.name_span,
                format!(
                    "control falls into `{}`, which does not head a region — the document \
                     did not check cleanly",
                    target.name
                ),
            )
        })?;
        let mut args = Vec::new();
        for name in flow.plan.region_params(region) {
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
                callee: flow.fns.regions[region],
                args,
            },
        });
    }
    match &flow.exit {
        Some(FlowExit { binding, .. }) => {
            let bound = scope.get(binding).ok_or_else(|| {
                LowerError::new(
                    step.name_span,
                    format!("the collected binding `{binding}` is not in scope at the flow's end"),
                )
            })?;
            let ok = ctx.atom("ok");
            let value = record_new(ctx, ok, vec![Value::Var(bound.var)], &mut stmts);
            Ok(Block {
                stmts,
                tail: Tail::Return(Value::Var(value)),
            })
        }
        None => Err(LowerError::unsupported(
            "step falls through without a route",
            step.name_span,
        )),
    }
}

pub(super) fn lower_statement(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    step: &Step,
    statement: &Statement,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Option<Tail>, LowerError> {
    match statement {
        Statement::Call(call) => {
            lower_call(ctx, env, call, scope, stmts)?;
            Ok(None)
        }
        Statement::Sleep(sleep) => {
            lower_sleep(ctx, sleep.duration.magnitude, sleep.duration.unit, stmts);
            Ok(None)
        }
        Statement::Pipe(pipe) => {
            let (value, ty) = super::pipes::lower_pipe_value(
                ctx,
                env.plan,
                &pipe.head,
                &pipe.stages,
                scope,
                stmts,
            )?;
            match &pipe.end {
                PipeEnd::Bind(binding) => {
                    let var = as_var(ctx, value, stmts);
                    scope.insert(binding.name.clone(), Binding { var, ty });
                    Ok(None)
                }
                PipeEnd::Route(target) => {
                    let tail = route_tail(ctx, env, target, scope, Some((value, ty)), stmts)?;
                    Ok(Some(tail))
                }
            }
        }
        Statement::Route(route) => {
            let tail = route_tail(ctx, env, &route.target, scope, None, stmts)?;
            Ok(Some(tail))
        }
        Statement::Spawn(spawn) => {
            super::child_call::lower_spawn_stmt(ctx, env.plan, spawn, scope, stmts)?;
            Ok(None)
        }
        Statement::Wait(wait) => {
            super::wait::lower_wait_stmt(ctx, env.plan, step, wait, scope, stmts, slots)?;
            Ok(None)
        }
        Statement::Fork(fork) => {
            lower_fork_stmt(ctx, env, step, fork, scope, stmts, slots)?;
            Ok(None)
        }
        Statement::Loop(looped) => {
            lower_loop_stmt(ctx, env, step, looped, scope, stmts, slots)?;
            Ok(None)
        }
        Statement::SubStep(sub) => Err(LowerError::unsupported("substep", sub.name_span)),
        // The collapsed region step's fan-out pair: the header lowers the
        // whole delivery + collect; the collect marker is consumed.
        Statement::Distribute(distribute) => {
            super::fanout::lower_fanout(ctx, env, step, distribute, scope, stmts, slots)?;
            Ok(None)
        }
        Statement::Collect(_) => Ok(None),
    }
}

pub(super) fn lower_call(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    call: &CallStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(), LowerError> {
    // An awaited child call dispatches through the string-name spawn form;
    // config on a child call refuses with the reference's class
    // (`child_call::lower_child_call_stmt`).
    if ctx.emitter.children.contains_key(call.call.name.as_str()) {
        return super::child_call::lower_child_call_stmt(ctx, env.plan, call, scope, stmts);
    }
    let bound = activity_call(
        ctx,
        env.plan,
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

pub(super) fn as_var(ctx: &mut Ctx<'_>, value: Value, stmts: &mut Vec<Stmt>) -> Var {
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
