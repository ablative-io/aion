//! Route-cycle boundedness (rev-3 rule): control-flow successors are route
//! edges, fall-through edges, and `after` edges; a `max … visits` bound
//! must intersect EVERY directed cycle — one bounded member does not excuse
//! a sub-cycle that avoids it, so after removing the bounded vertices the
//! residual graph must be acyclic. A bounded `loop` inside a member never
//! satisfies the rule — the loop bounds its own iteration, not the step's
//! re-entry, so it was a decoy (soundness gap closed at rev 3). Sibling
//! substep groups form their own scoped graphs and answer to the same
//! rules.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Statement, Step};

use super::context::Ctx;
use super::graph::{
    Provenance, RouteEdge, collect_route_names, falls_through, find_after_cycle, report_after_cycle,
};

/// Report unbounded cycles; returns per-step membership of ANY cycle (used
/// by the flow walk to sanction cycle-threaded rebinding).
pub(super) fn check_route_cycles(
    ctx: &mut Ctx<'_>,
    steps: &[&Step],
    after: &[Vec<usize>],
    routes: &[RouteEdge],
    fall_pred: &[Option<usize>],
) -> Vec<bool> {
    // Control-flow successors: route edges, fall-through edges, and `after`
    // edges — a dependency's completion re-arms its dependents, so a
    // backward route plus a forward `after` edge is as unbounded a cycle as
    // two routes (decision log 2026-07-11).
    let count = steps.len();
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); count];
    for edge in routes {
        successors[edge.source].push(edge.target);
    }
    for (position, pred) in fall_pred.iter().enumerate() {
        if let Some(&source) = pred.as_ref() {
            successors[source].push(position);
        }
    }
    for (position, dependencies) in after.iter().enumerate() {
        for &dependency in dependencies {
            successors[dependency].push(position);
        }
    }

    // Full-graph cycle membership (the rebind sanction's notion of "on a
    // cycle" — boundedness does not change what a cycle threads).
    let component = strongly_connected(&successors);
    let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
    for &id in &component {
        *sizes.entry(id).or_insert(0) += 1;
    }
    let mut member_of_cycle = vec![false; count];
    for (position, &id) in component.iter().enumerate() {
        let self_edge = successors[position].contains(&position);
        member_of_cycle[position] = sizes.get(&id).copied().unwrap_or(0) > 1 || self_edge;
    }

    // The residual graph: drop every `max … visits` vertex. Any directed
    // cycle that survives has, by construction, no bound on it — a bound
    // must intersect EVERY cycle, not merely touch the SCC.
    let bounded: Vec<bool> = steps.iter().map(|step| step.max_visits.is_some()).collect();
    let mut residual: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (source, targets) in successors.iter().enumerate() {
        if bounded[source] {
            continue;
        }
        for &target in targets {
            if !bounded[target] {
                residual[source].push(target);
            }
        }
    }
    let residual_component = strongly_connected(&residual);
    let mut residual_sizes: BTreeMap<usize, usize> = BTreeMap::new();
    for (position, &id) in residual_component.iter().enumerate() {
        if !bounded[position] {
            *residual_sizes.entry(id).or_insert(0) += 1;
        }
    }
    let mut cyclic_residuals: BTreeSet<usize> = BTreeSet::new();
    for (position, &id) in residual_component.iter().enumerate() {
        if bounded[position] {
            continue;
        }
        let self_edge = residual[position].contains(&position);
        if residual_sizes.get(&id).copied().unwrap_or(0) > 1 || self_edge {
            cyclic_residuals.insert(id);
        }
    }
    for &id in &cyclic_residuals {
        report_unbounded(ctx, steps, routes, &residual_component, &bounded, id);
    }
    member_of_cycle
}

/// Report one residual cycle, anchored on the first backward (or self)
/// route edge inside it, falling back to any in-cycle route edge, then to
/// the earliest member step (a cycle of fall-through and `after` edges
/// alone).
fn report_unbounded(
    ctx: &mut Ctx<'_>,
    steps: &[&Step],
    routes: &[RouteEdge],
    residual_component: &[usize],
    bounded: &[bool],
    id: usize,
) {
    let members: Vec<usize> = residual_component
        .iter()
        .enumerate()
        .filter_map(|(position, &component)| {
            (component == id && !bounded[position]).then_some(position)
        })
        .collect();
    let in_cycle = |edge: &&RouteEdge| -> bool {
        !bounded[edge.source]
            && !bounded[edge.target]
            && residual_component[edge.source] == id
            && residual_component[edge.target] == id
    };
    let anchor = routes
        .iter()
        .filter(in_cycle)
        .filter(|edge| edge.target <= edge.source)
        .min_by_key(|edge| (edge.source, edge.span.start))
        .or_else(|| {
            routes
                .iter()
                .filter(in_cycle)
                .min_by_key(|edge| (edge.source, edge.span.start))
        });
    if let Some(edge) = anchor {
        let target = &steps[edge.target].name;
        ctx.error(
            edge.span,
            format!(
                "routing to `{target}` forms a cycle with no bound: some step in the \
                 cycle must carry a `max … visits` re-entry bound (unbounded cycles \
                 are unwritable; a bounded `loop` inside a member is not a cycle bound)"
            ),
        );
    } else if let Some(&first) = members.first() {
        let names: Vec<&str> = members
            .iter()
            .map(|&member| steps[member].name.as_str())
            .collect();
        ctx.error(
            steps[first].name_span,
            format!(
                "steps {} re-arm each other (fall-through and `after` edges) in a \
                 cycle with no bound: some step in the cycle must carry a \
                 `max … visits` re-entry bound (unbounded cycles are unwritable)",
                names.join(" -> ")
            ),
        );
    }
}

/// Sibling substeps route among themselves, so every sibling group is its
/// own scoped control-flow graph: run the same cycle/bound analysis over
/// it, recursively (substeps nest through bodies, forks, loops, and
/// `on failure` blocks).
pub(super) fn check_substep_cycles(ctx: &mut Ctx<'_>, steps: &[Step]) {
    for step in steps {
        scan_statement_lists(ctx, step);
    }
}

fn scan_statement_lists(ctx: &mut Ctx<'_>, step: &Step) {
    scan_list(ctx, &step.body);
    if let Some(on_failure) = &step.on_failure {
        scan_list(ctx, &on_failure.body);
    }
}

fn scan_list(ctx: &mut Ctx<'_>, statements: &[Statement]) {
    let siblings: Vec<&Step> = statements
        .iter()
        .filter_map(|statement| match statement {
            Statement::SubStep(sub) => Some(sub.as_ref()),
            _ => None,
        })
        .collect();
    if !siblings.is_empty() {
        check_sibling_group(ctx, &siblings);
    }
    for statement in statements {
        match statement {
            Statement::SubStep(sub) => scan_statement_lists(ctx, sub),
            Statement::Fork(fork) => scan_list(ctx, &fork.body),
            Statement::Loop(looped) => scan_list(ctx, &looped.body),
            _ => {}
        }
    }
}

/// One sibling group: route edges between siblings plus the fall-through
/// chain (a sibling with no exits hands control to the next, exactly like
/// top-level steps), analyzed under the same rule set and diagnostics.
fn check_sibling_group(ctx: &mut Ctx<'_>, siblings: &[&Step]) {
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for (position, sibling) in siblings.iter().enumerate() {
        index.entry(sibling.name.as_str()).or_insert(position);
    }
    // Resolve `after` targets within the sibling scope, exactly as the
    // top-level graph pass does — unknown names are defects, and the
    // dependency edges join the cycle analysis below.
    let mut after: Vec<Vec<usize>> = vec![Vec::new(); siblings.len()];
    for (position, sibling) in siblings.iter().enumerate() {
        for dependency in &sibling.after {
            if let Some(&target) = index.get(dependency.name.as_str()) {
                ctx.semantic
                    .reference_to(dependency.span, Some(siblings[target].name_span));
                after[position].push(target);
            } else {
                ctx.error(
                    dependency.span,
                    format!(
                        "step `{}` declares `after {}`, but no step named `{}` exists \
                         among its sibling substeps",
                        sibling.name, dependency.name, dependency.name
                    ),
                );
            }
        }
    }
    if let Some(cycle) = find_after_cycle(&after) {
        report_after_cycle(ctx, siblings, &cycle);
        return;
    }
    let mut routes: Vec<RouteEdge> = Vec::new();
    for (position, sibling) in siblings.iter().enumerate() {
        for (name, span) in collect_route_names(sibling) {
            if let Some(&target) = index.get(name) {
                routes.push(RouteEdge {
                    source: position,
                    target,
                    span,
                    provenance: Provenance::Success,
                });
            }
        }
    }
    let mut route_targeted = vec![false; siblings.len()];
    for edge in &routes {
        route_targeted[edge.target] = true;
    }
    let mut fall_pred: Vec<Option<usize>> = vec![None; siblings.len()];
    for position in 1..siblings.len() {
        if after[position].is_empty()
            && !route_targeted[position]
            && falls_through(siblings[position - 1])
        {
            fall_pred[position] = Some(position - 1);
        }
    }
    check_route_cycles(ctx, siblings, &after, &routes, &fall_pred);
}

/// Kosaraju strongly-connected components; returns a component id per node.
fn strongly_connected(successors: &[Vec<usize>]) -> Vec<usize> {
    let count = successors.len();
    let mut order = Vec::with_capacity(count);
    let mut seen = vec![false; count];
    for start in 0..count {
        if seen[start] {
            continue;
        }
        // Iterative post-order.
        let mut stack = vec![(start, 0_usize)];
        seen[start] = true;
        while let Some(&mut (node, ref mut next)) = stack.last_mut() {
            if *next < successors[node].len() {
                let child = successors[node][*next];
                *next += 1;
                if !seen[child] {
                    seen[child] = true;
                    stack.push((child, 0));
                }
            } else {
                order.push(node);
                stack.pop();
            }
        }
    }
    let mut reversed: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (source, targets) in successors.iter().enumerate() {
        for &target in targets {
            reversed[target].push(source);
        }
    }
    let mut component = vec![usize::MAX; count];
    let mut current = 0;
    for &start in order.iter().rev() {
        if component[start] != usize::MAX {
            continue;
        }
        let mut stack = vec![start];
        component[start] = current;
        while let Some(node) = stack.pop() {
            for &pred in &reversed[node] {
                if component[pred] == usize::MAX {
                    component[pred] = current;
                    stack.push(pred);
                }
            }
        }
        current += 1;
    }
    component
}
