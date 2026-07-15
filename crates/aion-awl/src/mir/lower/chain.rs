//! Chain-boundary name liveness for multi-step sequential regions: each
//! non-entry chain step lowers to its own `FlowFn`, reached by the previous
//! step's tail call (IR-14), so its parameter list is the set of names live at
//! that boundary. This is the shared plan's fixed point restricted to a
//! straight-line chain — refs/defs collection follows the reference
//! (`emitter/liveness.rs` `collect_into`/`collect_route`) except for the
//! loop-body-bind asymmetry documented at the loop arm below. A route to
//! another region contributes that region's `region_params` exactly as the
//! fixed point's `params(callee) − defs` term does; a route to a nested
//! flow's exit contributes the exit contract (the collected binding, or a
//! subflow outcome's bare pickup), mirroring the shared collector's
//! `target_refs_form`. A chain whose last step falls through seeds the
//! backward walk with the demand of the implicit hand-off (the next step's
//! region params, or a member flow's collected binding). The shared collector
//! cannot be consumed directly here: it intentionally aggregates an entire
//! region into one graph node, while chain splitting needs a live-in set at
//! every interior step boundary. Substeps remain outside the covered subset
//! and collect nothing here because they refuse during body lowering.

use std::collections::BTreeSet;

use crate::ast::{Expr, Guard, PipeEnd, PipeStage, RoutePayload, RouteTarget, Statement, Step};
use crate::emitter::{expr_refs, falls_through, visits_counter};

use super::ctx::Ctx;
use super::flow::{ExitKind, FlowCtx, FlowExit};

/// Per-position parameter names for a sequential chain, in chain order. The
/// entry position is computed too (it equals the region's params for a chain
/// the shared fixed point saw), but callers keep `region_params` for the entry
/// so the parity anchor stays the shared plan.
pub(super) fn chain_params(
    ctx: &Ctx<'_>,
    flow: &FlowCtx<'_>,
    chain: &[usize],
) -> Vec<Vec<String>> {
    let steps = flow.steps;
    let mut live: BTreeSet<String> = BTreeSet::new();
    // Seed with the implicit end-of-chain demand: falling through into the
    // next step's region, or a member flow's exit return of the collected
    // binding (the reference `emit_flow_end` / the shared collector's
    // fall-through edge).
    if let Some(&last) = chain.last()
        && falls_through(&steps[last])
    {
        let next = last + 1;
        if next < steps.len() {
            if let Some(region) = flow.plan.region_of_entry(next) {
                live.extend(flow.plan.region_params(region).iter().cloned());
            }
        } else if let Some(FlowExit {
            kind: ExitKind::Region { binding },
            ..
        }) = &flow.exit
        {
            live.insert(binding.clone());
        }
    }
    let mut params = vec![Vec::new(); chain.len()];
    for position in (0..chain.len()).rev() {
        step_live_in(ctx, flow, &steps[chain[position]], &mut live);
        params[position] = live.iter().cloned().collect();
    }
    params
}

/// Fold one step: `live = refs(step) ∪ (live − defs(step))`, with refs
/// collected use-before-def in written order.
fn step_live_in(ctx: &Ctx<'_>, flow: &FlowCtx<'_>, step: &Step, live: &mut BTreeSet<String>) {
    let mut refs: BTreeSet<String> = BTreeSet::new();
    let mut defs: BTreeSet<String> = BTreeSet::new();
    // A bounded step reads its language-owned visit counter and its bound
    // expression at entry, before any of its defs (the shared collector's
    // `collect_step`).
    if let Some(max_visits) = &step.max_visits {
        refs.insert(visits_counter(&step.name));
        add_expr(&max_visits.bound, &defs, &mut refs);
    }
    // A collapsed per-item region step reads every free name its member
    // flow's wrapper threads in (beyond the per-item variable).
    if matches!(step.body.first(), Some(Statement::Distribute(_)))
        && let Some(region) = flow.regions.get(&step.name)
        && let Some(nested) = ctx.plans.regions.get(&region.id)
    {
        for name in &nested.wrapper_params {
            if name != &region.var {
                refs.insert(name.clone());
            }
        }
    }
    collect_statements(ctx, flow, &step.body, &mut defs, &mut refs);
    for clause in &step.outcomes {
        if let Guard::When { expr, .. } = &clause.guard {
            add_expr(expr, &defs, &mut refs);
        }
        route_refs(ctx, flow, &clause.route, &defs, &mut refs, true);
    }
    for def in &defs {
        live.remove(def);
    }
    live.extend(refs);
}

fn collect_statements(
    ctx: &Ctx<'_>,
    flow: &FlowCtx<'_>,
    statements: &[Statement],
    defs: &mut BTreeSet<String>,
    refs: &mut BTreeSet<String>,
) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                for arg in &call.call.args {
                    add_expr(&arg.value, defs, refs);
                }
                if let Some(bind) = &call.bind {
                    defs.insert(bind.name.clone());
                }
            }
            Statement::Spawn(spawn) => {
                for arg in &spawn.call.args {
                    add_expr(&arg.value, defs, refs);
                }
            }
            Statement::Pipe(pipe) => {
                add_expr(&pipe.head, defs, refs);
                for stage in &pipe.stages {
                    if let PipeStage::Combinator(combinator) = stage
                        && let Some(arg) = &combinator.arg
                    {
                        add_expr(arg, defs, refs);
                    }
                }
                match &pipe.end {
                    PipeEnd::Bind(binding) => {
                        defs.insert(binding.name.clone());
                    }
                    PipeEnd::Route(target) => {
                        // A piped route carries the piped value as its
                        // payload: no bare-outcome pickup (`collect_pipe`).
                        route_refs(ctx, flow, target, defs, refs, false);
                    }
                }
            }
            Statement::Route(route) => {
                route_refs(ctx, flow, &route.target, defs, refs, true);
            }
            Statement::Wait(wait) => {
                defs.insert(wait.bind.name.clone());
            }
            // Boundary-shaped like the shared collector's `collect_loop`:
            // seed and `max` read the pre-loop scope; the threaded var and
            // counter are local while collecting the body/`until`. Deliberate
            // asymmetry: shared `collect_into` registers every `loop_defs`
            // entry (including body binds) as a step def; this chain boundary
            // keeps body binds local because the checker forbids them after
            // the loop. Only the threaded value and counter escape here.
            Statement::Loop(looped) => {
                add_expr(&looped.seed, defs, refs);
                if let Some(max) = &looped.max {
                    add_expr(&max.expr, defs, refs);
                }
                let mut loop_defs = defs.clone();
                loop_defs.insert(looped.var.clone());
                if let Some(counter) = &looped.counter {
                    loop_defs.insert(counter.name.clone());
                }
                collect_statements(ctx, flow, &looped.body, &mut loop_defs, refs);
                if let Some(until) = &looped.until {
                    add_expr(&until.expr, &loop_defs, refs);
                }
                defs.insert(looped.var.clone());
                if let Some(counter) = &looped.counter {
                    defs.insert(counter.name.clone());
                }
            }
            Statement::Fork(fork) => {
                if let crate::ast::ForkHeader::Collection {
                    var, collection, ..
                } = &fork.header
                {
                    add_expr(collection, defs, refs);
                    let mut branch_defs = defs.clone();
                    branch_defs.insert(var.clone());
                    collect_statements(ctx, flow, &fork.body, &mut branch_defs, refs);
                } else {
                    // Named branches merge their bindings at the join.
                    collect_statements(ctx, flow, &fork.body, defs, refs);
                }
                if let Some(bind) = &fork.join.bind {
                    defs.insert(bind.name.clone());
                }
            }
            // The fan-out pair of a collapsed region step: the header reads
            // the collection, the collect defines the gathered binding
            // (member free names fold in `step_live_in`).
            Statement::Distribute(distribute) => {
                add_expr(&distribute.collection, defs, refs);
            }
            Statement::Collect(collect) => {
                defs.insert(collect.bind.name.clone());
            }
            // Sleeps reference no bindings. Substeps refuse during body
            // lowering and therefore never reach chain materialization.
            Statement::Sleep(_) | Statement::SubStep(_) => {}
        }
    }
}

/// A route's name demand: payload argument (or value expression) refs, the
/// exit contract of a nested flow, the bare-route outcome pickup
/// (`Statement::Route` only — a piped route carries its payload), or — for a
/// route to another region's entry — that region's parameter list (the fixed
/// point's cross-function term).
fn route_refs(
    ctx: &Ctx<'_>,
    flow: &FlowCtx<'_>,
    target: &RouteTarget,
    defs: &BTreeSet<String>,
    refs: &mut BTreeSet<String>,
    bare_pickup: bool,
) {
    for arg in target.payload_args() {
        add_expr(&arg.value, defs, refs);
    }
    if let Some(RoutePayload::Value(value)) = &target.payload {
        add_expr(value, defs, refs);
    }
    if let Some(exit) = &flow.exit
        && exit.name == target.name
    {
        match &exit.kind {
            ExitKind::Region { binding } => {
                if !defs.contains(binding) {
                    refs.insert(binding.clone());
                }
            }
            ExitKind::Subflow { .. } => {
                if target.payload.is_none() && bare_pickup && !defs.contains(target.name.as_str()) {
                    refs.insert(target.name.clone());
                }
            }
        }
        return;
    }
    if flow.exit.is_none() && ctx.emitter.outcomes.contains_key(target.name.as_str()) {
        if target.payload.is_none() && bare_pickup && !defs.contains(target.name.as_str()) {
            refs.insert(target.name.clone());
        }
        return;
    }
    if target.payload.is_some() {
        return;
    }
    let Some(step_index) = flow
        .steps
        .iter()
        .position(|step| step.name == target.name)
    else {
        return;
    };
    let Some(region) = flow.plan.region_of_entry(step_index) else {
        return;
    };
    for name in flow.plan.region_params(region) {
        if !defs.contains(name) {
            refs.insert(name.clone());
        }
    }
}

fn add_expr(expr: &Expr, defs: &BTreeSet<String>, refs: &mut BTreeSet<String>) {
    let mut found = BTreeSet::new();
    expr_refs(expr, &mut found);
    for name in found {
        if !defs.contains(&name) {
            refs.insert(name);
        }
    }
}
