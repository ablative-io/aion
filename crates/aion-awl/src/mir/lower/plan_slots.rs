//! The flow-function slot inventory (split from `build` for the 500-line
//! law), extended by B4 to every nested flow.
//!
//! Canonical slot order (byte-stability contract: a module without rev-3
//! constructs has no nested flows and no `distribute` statements, so every
//! pre-B4 `FnRef` is preserved exactly):
//!
//! 1. host region chains (one slot per layer, as always);
//! 2. nested flow function sets — subflows in declaration order, then
//!    per-item regions by ascending region id; each set is the run-once
//!    entry wrapper slot followed by that flow's region-chain slots;
//! 3. loop slots — host steps first (plan-region order, layers flattened,
//!    `loops::count_loops` per body), then each nested flow's steps in the
//!    same nested-flow order;
//! 4. fork slots — the same extended traversal, now counting BOTH `fork`
//!    statements (`forks::count_fork_fns`) and the fan-out lifted closures
//!    of a collapsed region step (`fanout::count_fanout_fns` — the
//!    `distribute`/`sequence` marker heads the synthetic body, so its
//!    closures precede any later fork's in the same step);
//! 5. wait slots — the same extended traversal;
//! 6. the fixed helpers (T-DEAD, T-WIT) and dynamic predicates, as always.

use std::collections::BTreeMap;

use crate::ast::Step;
use crate::emitter::{Plan, RegionShape};

use super::super::ids::FnRef;
use super::build::{FlowFns, NestedFns};
use super::ctx::Ctx;
use super::driver::LowerError;

/// The reserved flow-function slots: per-flow chain tables, then the shared
/// loop/fork/wait pools.
pub(super) struct FlowSlots {
    pub(super) host: FlowFns,
    pub(super) subflow_fns: BTreeMap<String, NestedFns>,
    pub(super) region_fns: BTreeMap<usize, NestedFns>,
    pub(super) loops: Vec<FnRef>,
    pub(super) forks: Vec<FnRef>,
    pub(super) waits: Vec<FnRef>,
    pub(super) child_witness_needed: bool,
}

/// One flow's traversal surface, in the canonical order (host, subflows by
/// declaration, regions by id).
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
    for shape in emitter.subflow_shapes {
        let nested = plans
            .subflows
            .get(&shape.name)
            .ok_or_else(|| LowerError::Planning {
                message: format!("subflow `{}` was never planned", shape.name),
            })?;
        flows.push(FlowWalk {
            steps: &shape.flow.steps,
            regions: &shape.flow.regions,
            plan: &nested.plan,
        });
    }
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
    let mut subflow_fns = BTreeMap::new();
    for shape in emitter.subflow_shapes {
        let nested = plans
            .subflows
            .get(&shape.name)
            .ok_or_else(|| LowerError::Planning {
                message: format!("subflow `{}` was never planned", shape.name),
            })?;
        let wrapper = FnRef(*next);
        *next += 1;
        let fns = chain_slots(&nested.plan, next);
        subflow_fns.insert(shape.name.clone(), NestedFns { wrapper, fns });
    }
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
            child_witness_needed |= super::forks::needs_child_witness(&step.body, emitter);
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
    Ok(FlowSlots {
        host,
        subflow_fns,
        region_fns,
        loops,
        forks,
        waits,
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
