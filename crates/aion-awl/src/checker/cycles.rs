//! Route-cycle boundedness (rev-3 rule): control-flow successors are route
//! edges, fall-through edges, and `after` edges; a cycle is legal iff a
//! member step carries a `max … visits` re-entry bound (an input-derived
//! expression or a literal). A bounded `loop` inside a member no longer
//! satisfies the rule — the loop bounds its own iteration, not the step's
//! re-entry, so it was a decoy (soundness gap closed at rev 3).

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::Step;

use super::context::Ctx;
use super::graph::RouteEdge;

/// Report unbounded cycles; returns per-step membership of ANY cycle (used
/// by the flow walk to sanction cycle-threaded rebinding).
pub(super) fn check_route_cycles(
    ctx: &mut Ctx<'_>,
    steps: &[Step],
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
    let component = strongly_connected(&successors);
    let mut cyclic: BTreeSet<usize> = BTreeSet::new();
    let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
    for &id in &component {
        *sizes.entry(id).or_insert(0) += 1;
    }
    for (position, &id) in component.iter().enumerate() {
        let self_route = routes
            .iter()
            .any(|edge| edge.source == position && edge.target == position);
        if sizes.get(&id).copied().unwrap_or(0) > 1 || self_route {
            cyclic.insert(id);
        }
    }
    let mut member_of_cycle = vec![false; count];
    for (position, &id) in component.iter().enumerate() {
        member_of_cycle[position] = cyclic.contains(&id);
    }
    for &id in &cyclic {
        let members: Vec<usize> = component
            .iter()
            .enumerate()
            .filter_map(|(position, &c)| (c == id).then_some(position))
            .collect();
        if members
            .iter()
            .any(|&member| steps[member].max_visits.is_some())
        {
            continue;
        }
        // Anchor on the first backward (or self) route edge inside the
        // cycle, falling back to any in-cycle route edge, then to the
        // earliest member step (a cycle of fall-through and `after` edges
        // alone).
        let in_cycle = |edge: &&RouteEdge| -> bool {
            component[edge.source] == id && component[edge.target] == id
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
    member_of_cycle
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
