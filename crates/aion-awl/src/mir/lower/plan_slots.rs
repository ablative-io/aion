//! The flow-function slot inventory (split from `build` for the 500-line
//! law), extended by the B4/B5 parity landing to every nested flow.
//!
//! Canonical slot order (byte-stability contract: a module without rev-3
//! constructs has no nested flows, no `distribute` statements, and no child
//! adapters, so every pre-B4 `FnRef` is preserved exactly):
//!
//! 1. host region chains (one slot per layer, as always);
//! 2. nested flow function sets — per-item regions by ascending region id;
//!    each set is the run-once entry wrapper slot followed by that flow's
//!    region-chain slots;
//! 3. loop slots — host steps first (plan-region order, layers flattened,
//!    `loops::count_loops` per body), then each nested flow's steps in the
//!    same nested-flow order;
//! 4. fork slots — the same extended traversal, now counting BOTH `fork`
//!    statements (`forks::count_fork_fns`) and the fan-out lifted closures
//!    of a collapsed region step (`fanout::count_fanout_fns` — the
//!    `distribute`/`sequence` marker heads the synthetic body, so its
//!    closures precede any later fork's in the same step);
//! 5. wait slots — the same extended traversal;
//! 6. implicit-child adapter shells (`_execute` then `_run`), ascending
//!    region id, one pair per region that runs as a synthesized child;
//! 7. the fixed helpers (T-DEAD, T-WIT) and dynamic predicates, as always.

use std::collections::BTreeMap;

use crate::ast::Step;
use crate::emitter::{Plan, RegionShape, implicit_child_required};

use super::super::ids::FnRef;
use super::build::{AdapterFns, FlowFns, NestedFns};
use super::ctx::Ctx;
use super::driver::LowerError;

/// The reserved flow-function slots: per-flow chain tables, the shared
/// loop/fork/wait pools, and the implicit-child adapter pairs.
pub(super) struct FlowSlots {
    pub(super) host: FlowFns,
    pub(super) region_fns: BTreeMap<usize, NestedFns>,
    pub(super) loops: Vec<FnRef>,
    pub(super) forks: Vec<FnRef>,
    pub(super) waits: Vec<FnRef>,
    pub(super) adapters: BTreeMap<usize, AdapterFns>,
    pub(super) child_witness_needed: bool,
}

/// One flow's traversal surface, in the canonical order (host, then regions
/// by ascending id).
struct FlowWalk<'a> {
    steps: &'a [Step],
    regions: &'a BTreeMap<String, RegionShape>,
    plan: &'a Plan,
}

/// Every flow in the canonical nested-flow order.
fn flows_in_order<'c>(ctx: &Ctx<'c>) -> Result<Vec<FlowWalk<'c>>, LowerError> {
    let emitter = ctx.emitter;
    let plans = ctx.plans;
    let mut flows = vec![FlowWalk {
        steps: &emitter.document.steps,
        regions: emitter.host_regions,
        plan: &plans.host,
    }];
    for (&id, nested) in &plans.regions {
        let shape = plans
            .region_shapes
            .get(&id)
            .ok_or_else(|| LowerError::Planning {
                message: format!("region {id} lost its shape"),
            })?;
        flows.push(FlowWalk {
            steps: &shape.members.steps,
            regions: &shape.members.regions,
            plan: &nested.plan,
        });
    }
    Ok(flows)
}

/// One flow's region-chain slots (one per layer, min one).
fn chain_slots(plan: &Plan, next: &mut u32) -> FlowFns {
    let mut regions = Vec::new();
    let mut chains = Vec::new();
    for region in &plan.regions {
        regions.push(FnRef(*next));
        let slots = region.layers.len().max(1);
        let mut chain = Vec::with_capacity(slots);
        for _ in 0..slots {
            chain.push(FnRef(*next));
            *next += 1;
        }
        chains.push(chain);
    }
    FlowFns { regions, chains }
}

/// Reserve every flow-function slot in the canonical order documented in the
/// module header.
pub(super) fn plan_flow_slots(ctx: &Ctx<'_>, next: &mut u32) -> Result<FlowSlots, LowerError> {
    let emitter = ctx.emitter;
    let plans = ctx.plans;
    let host = chain_slots(&plans.host, next);
    let mut region_fns = BTreeMap::new();
    for (&id, nested) in &plans.regions {
        let wrapper = FnRef(*next);
        *next += 1;
        let fns = chain_slots(&nested.plan, next);
        region_fns.insert(id, NestedFns { wrapper, fns });
    }

    let flows = flows_in_order(ctx)?;
    let mut loops = Vec::new();
    for flow in &flows {
        for_each_step(flow, |step| {
            for _ in 0..super::loops::count_loops(&step.body) {
                loops.push(FnRef(*next));
                *next += 1;
            }
        });
    }
    let mut forks = Vec::new();
    let mut child_witness_needed = false;
    for flow in &flows {
        for_each_step(flow, |step| {
            child_witness_needed |= super::forks::needs_child_witness(&step.body, emitter)
                || super::fanout::needs_child_witness(step, flow.regions, emitter);
            let count = super::fanout::count_fanout_fns(step, flow.regions, emitter)
                + super::forks::count_fork_fns(&step.body, emitter);
            for _ in 0..count {
                forks.push(FnRef(*next));
                *next += 1;
            }
        });
    }
    let mut waits = Vec::new();
    for flow in &flows {
        for_each_step(flow, |step| {
            for _ in 0..super::wait::count_wait_fns(&step.body) {
                waits.push(FnRef(*next));
                *next += 1;
            }
        });
    }
    let mut adapters = BTreeMap::new();
    for &id in plans.regions.keys() {
        let Some(&shape) = plans.region_shapes.get(&id) else {
            continue;
        };
        if !implicit_child_required(emitter, shape) {
            continue;
        }
        let execute = FnRef(*next);
        let run = FnRef(*next + 1);
        *next += 2;
        adapters.insert(id, AdapterFns { execute, run });
    }
    Ok(FlowSlots {
        host,
        region_fns,
        loops,
        forks,
        waits,
        adapters,
        child_witness_needed,
    })
}

/// Walk one flow's steps in lowering order: plan regions in order, layers
/// flattened, written order within a layer.
fn for_each_step(flow: &FlowWalk<'_>, mut f: impl FnMut(&Step)) {
    for region in &flow.plan.regions {
        for &step_index in region.layers.iter().flatten() {
            if let Some(step) = flow.steps.get(step_index) {
                f(step);
            }
        }
    }
}

/// T-DEAD precedes T-WIT; dynamically generated predicates follow both. This
/// preserves every existing function reference when a module has no predicate.
pub(super) fn fixed_helper_refs(
    next: u32,
    has_activity: bool,
    child_witness_needed: bool,
) -> (Option<FnRef>, FnRef) {
    let dead_offset = u32::from(has_activity);
    let child_witness = child_witness_needed.then_some(FnRef(next + dead_offset));
    let fixed_count = dead_offset + u32::from(child_witness_needed);
    (child_witness, FnRef(next + fixed_count))
}
