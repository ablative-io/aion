//! Chain-boundary name liveness for multi-step sequential regions: each
//! non-entry chain step lowers to its own `FlowFn`, reached by the previous
//! step's tail call (IR-14), so its parameter list is the set of names live at
//! that boundary. This is the shared plan's fixed point restricted to a
//! straight-line chain — refs/defs collection follows the reference
//! (`emitter/liveness.rs` `collect_into`/`collect_route`) except for the
//! loop-body-bind asymmetry documented at the loop arm below. A route to
//! another region contributes that region's `region_params` exactly as the
//! fixed point's `params(callee) − defs` term does. The shared collector cannot
//! be consumed directly here: it intentionally aggregates an entire region
//! into one graph node, while chain splitting needs a live-in set at every
//! interior step boundary. Exposing those folds would widen the planner API and
//! still require the chain-specific backward walk. Statement forms outside
//! the covered subset (forks, substeps) collect nothing here — their
//! statements refuse during body lowering, so no module carrying them ever
//! materializes.

use std::collections::BTreeSet;

use crate::ast::{Expr, Guard, PipeEnd, PipeStage, RouteTarget, Statement, Step};
use crate::emitter::{Emitter, Plan, expr_refs};

/// Per-position parameter names for a sequential chain, in chain order. The
/// entry position is computed too (it equals the region's params for a chain
/// the shared fixed point saw), but callers keep `region_params` for the entry
/// so the parity anchor stays the shared plan.
pub(super) fn chain_params(
    emitter: &Emitter<'_>,
    plan: &Plan,
    chain: &[usize],
) -> Vec<Vec<String>> {
    let steps = &emitter.document.steps;
    let mut live: BTreeSet<String> = BTreeSet::new();
    let mut params = vec![Vec::new(); chain.len()];
    for position in (0..chain.len()).rev() {
        step_live_in(emitter, plan, &steps[chain[position]], &mut live);
        params[position] = live.iter().cloned().collect();
    }
    params
}

/// Fold one step: `live = refs(step) ∪ (live − defs(step))`, with refs
/// collected use-before-def in written order.
fn step_live_in(emitter: &Emitter<'_>, plan: &Plan, step: &Step, live: &mut BTreeSet<String>) {
    let mut refs: BTreeSet<String> = BTreeSet::new();
    let mut defs: BTreeSet<String> = BTreeSet::new();
    collect_statements(emitter, plan, &step.body, &mut defs, &mut refs);
    for clause in &step.outcomes {
        if let Guard::When { expr, .. } = &clause.guard {
            add_expr(expr, &defs, &mut refs);
        }
        route_refs(emitter, plan, &clause.route, &defs, &mut refs, true);
    }
    for def in &defs {
        live.remove(def);
    }
    live.extend(refs);
}

fn collect_statements(
    emitter: &Emitter<'_>,
    plan: &Plan,
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
                        route_refs(emitter, plan, target, defs, refs, false);
                    }
                }
            }
            Statement::Route(route) => {
                route_refs(emitter, plan, &route.target, defs, refs, true);
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
                collect_statements(emitter, plan, &looped.body, &mut loop_defs, refs);
                if let Some(until) = &looped.until {
                    add_expr(&until.expr, &loop_defs, refs);
                }
                defs.insert(looped.var.clone());
                if let Some(counter) = &looped.counter {
                    defs.insert(counter.name.clone());
                }
            }
            // Forks, substeps, and sleeps contribute nothing: sleeps
            // reference no bindings, the rest refuse during body lowering.
            Statement::Sleep(_) | Statement::Fork(_) | Statement::SubStep(_) => {}
        }
    }
}

/// A route's name demand: payload argument refs, the bare-route outcome
/// pickup (`Statement::Route` only — a piped route carries its payload), or —
/// for a route to another region's entry — that region's parameter list (the
/// fixed point's cross-function term).
fn route_refs(
    emitter: &Emitter<'_>,
    plan: &Plan,
    target: &RouteTarget,
    defs: &BTreeSet<String>,
    refs: &mut BTreeSet<String>,
    bare_pickup: bool,
) {
    if let Some(payload) = &target.payload {
        for arg in payload {
            add_expr(&arg.value, defs, refs);
        }
        return;
    }
    if emitter.outcomes.contains_key(target.name.as_str()) {
        if bare_pickup && !defs.contains(target.name.as_str()) {
            refs.insert(target.name.clone());
        }
        return;
    }
    let Some(step_index) = emitter
        .document
        .steps
        .iter()
        .position(|step| step.name == target.name)
    else {
        return;
    };
    let Some(region) = plan.region_of_entry(step_index) else {
        return;
    };
    for name in plan.region_params(region) {
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
