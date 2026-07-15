//! Graph pass, run once per flow (the workflow's steps, then each
//! subflow's): `after` target resolution and dependency-cycle detection,
//! per-item region analysis (formation, placement, routing, definite
//! assignment), reachability, the final-step explicit-route rule,
//! route-cycle boundedness, and the binding-availability dataflow (a
//! binding is readable only where it is guaranteed on every path into a
//! step).

use std::collections::{BTreeMap, BTreeSet};

use crate::Span;
use crate::ast::{PipeEnd, Statement, Step};

use super::avail::{Origins, availability, universe};
use super::context::{Ctx, Flow};
use super::{cycles, regions};

/// One `route` edge between top-level steps.
pub(super) struct RouteEdge {
    /// Source step index.
    pub(super) source: usize,
    /// Target step index.
    pub(super) target: usize,
    /// Span of the route target name.
    pub(super) span: Span,
}

/// The analyzed step graph handed to the flow walk.
pub(super) struct StepGraph {
    /// Bindings guaranteed on every path into each step.
    pub(super) avail_in: Vec<BTreeSet<String>>,
    /// Checker-resolved declaration origins for guaranteed bindings.
    pub(super) origins_in: Vec<Origins>,
    /// Whether an `after` dependency cycle was found (flow walk is skipped).
    pub(super) after_cycle: bool,
    /// Per-step membership of any route cycle (sanctions cycle-threaded
    /// rebinding in the flow walk).
    pub(super) cyclic: Vec<bool>,
    /// Region-local names that fall out of scope at each `collect` step.
    pub(super) collect_masks: BTreeMap<usize, BTreeSet<String>>,
}

/// Build the step graph for one flow, reporting every graph-level
/// diagnostic.
pub(super) fn build(ctx: &mut Ctx<'_>, flow: &Flow<'_>) -> StepGraph {
    let steps = flow.steps;
    let count = steps.len();
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for (position, step) in steps.iter().enumerate() {
        if index.insert(step.name.as_str(), position).is_some() {
            ctx.error(step.name_span, format!("duplicate step `{}`", step.name));
        }
        if flow.outcomes.contains_key(&step.name) {
            ctx.error(
                step.name_span,
                format!(
                    "step `{}` shares its name with a workflow outcome — route targets \
                     live in one namespace, so `route {}` would be ambiguous; rename one",
                    step.name, step.name
                ),
            );
        }
    }

    // Region shape first: placement rules, bracket-nested formation, and
    // the semantic step kinds — all purely syntactic over the step list.
    regions::structure(ctx, flow);
    let formed = regions::form(ctx, flow);
    regions::classify(ctx, flow);
    let collect_masks = regions::masks(flow, &formed);

    // Resolve `after` targets.
    let mut after: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut after_unknown = vec![false; count];
    for (position, step) in steps.iter().enumerate() {
        for dependency in &step.after {
            if let Some(&target) = index.get(dependency.name.as_str()) {
                ctx.semantic
                    .reference_to(dependency.span, Some(steps[target].name_span));
                after[position].push(target);
            } else {
                after_unknown[position] = true;
                ctx.error(
                    dependency.span,
                    format!(
                        "step `{}` declares `after {}`, but no step named `{}` exists",
                        step.name, dependency.name, dependency.name
                    ),
                );
            }
        }
    }

    if let Some(cycle) = find_after_cycle(&after) {
        report_after_cycle(ctx, steps, &cycle);
        return StepGraph {
            avail_in: vec![universe(flow); count],
            origins_in: vec![Origins::new(); count],
            after_cycle: true,
            cyclic: vec![false; count],
            collect_masks,
        };
    }

    // Collect top-level route edges (routes inside substeps stay inside
    // their parent; the parent's own outcome clauses carry the exits).
    let mut routes: Vec<RouteEdge> = Vec::new();
    for (position, step) in steps.iter().enumerate() {
        for (name, span) in collect_route_names(step) {
            if let Some(&target) = index.get(name) {
                ctx.semantic
                    .reference_to(span, Some(steps[target].name_span));
                routes.push(RouteEdge {
                    source: position,
                    target,
                    span,
                });
            }
        }
    }
    let mut route_targeted = vec![false; count];
    for edge in &routes {
        route_targeted[edge.target] = true;
    }

    // Fall-through predecessor: a step with no `after` and no incoming
    // route depends on the step written immediately above it, when that
    // step can complete without routing away.
    let mut fall_pred: Vec<Option<usize>> = vec![None; count];
    for position in 1..count {
        if after[position].is_empty()
            && !route_targeted[position]
            && falls_through(&steps[position - 1])
        {
            fall_pred[position] = Some(position - 1);
        }
    }

    check_reachability(ctx, flow, &after, &after_unknown, &routes, &fall_pred);
    check_successors(ctx, steps, &after, &fall_pred);
    let cyclic = cycles::check_route_cycles(ctx, steps, &after, &routes, &fall_pred);
    regions::check_edges(ctx, flow, &formed);

    let (avail_in, origins_in) = availability(
        flow,
        &after,
        &after_unknown,
        &routes,
        &fall_pred,
        &collect_masks,
    );
    regions::check_collects(ctx, flow, &formed, &avail_in);
    StepGraph {
        avail_in,
        origins_in,
        after_cycle: false,
        cyclic,
        collect_masks,
    }
}

/// Whether a step can complete and hand control to the step below it: it has
/// no outcome clauses (which always route) and its body does not end in an
/// unconditional route.
pub(super) fn falls_through(step: &Step) -> bool {
    step.outcomes.is_empty() && !body_ends_in_route(&step.body)
}

/// Whether a statement list ends in an unconditional route (a `route` line
/// or a pipe chain terminating in `route`).
pub(super) fn body_ends_in_route(body: &[Statement]) -> bool {
    match body.last() {
        Some(Statement::Route(_)) => true,
        Some(Statement::Pipe(pipe)) => matches!(pipe.end, PipeEnd::Route(_)),
        _ => false,
    }
}

/// Every route target name written in a step's own surface: body statements
/// (recursing through forks and loops), the `on failure` block, and outcome
/// clauses. Substeps are excluded — their routes resolve within the parent.
pub(super) fn collect_route_names(step: &Step) -> Vec<(&str, Span)> {
    let mut found = Vec::new();
    collect_from_statements(&step.body, &mut found);
    if let Some(on_failure) = &step.on_failure {
        collect_from_statements(&on_failure.body, &mut found);
    }
    for clause in &step.outcomes {
        found.push((clause.route.name.as_str(), clause.route.name_span));
    }
    found
}

fn collect_from_statements<'a>(statements: &'a [Statement], found: &mut Vec<(&'a str, Span)>) {
    for statement in statements {
        match statement {
            Statement::Pipe(pipe) => {
                if let PipeEnd::Route(target) = &pipe.end {
                    found.push((target.name.as_str(), target.name_span));
                }
            }
            Statement::Route(route) => {
                found.push((route.target.name.as_str(), route.target.name_span));
            }
            Statement::Fork(fork) => collect_from_statements(&fork.body, found),
            Statement::Loop(looped) => collect_from_statements(&looped.body, found),
            Statement::Call(_)
            | Statement::Spawn(_)
            | Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::SubStep(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}

fn find_after_cycle(after: &[Vec<usize>]) -> Option<Vec<usize>> {
    // Three-color depth-first search over the `after` edges.
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;
    fn visit(
        node: usize,
        after: &[Vec<usize>],
        color: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        color[node] = GRAY;
        stack.push(node);
        for &next in &after[node] {
            if color[next] == GRAY {
                let start = stack.iter().position(|&member| member == next).unwrap_or(0);
                return Some(stack[start..].to_vec());
            }
            if color[next] == WHITE
                && let Some(cycle) = visit(next, after, color, stack)
            {
                return Some(cycle);
            }
        }
        stack.pop();
        color[node] = BLACK;
        None
    }
    let mut color = vec![WHITE; after.len()];
    for node in 0..after.len() {
        if color[node] == WHITE
            && let Some(cycle) = visit(node, after, &mut color, &mut vec![])
        {
            return Some(cycle);
        }
    }
    None
}

fn report_after_cycle(ctx: &mut Ctx<'_>, steps: &[Step], cycle: &[usize]) {
    let Some(&anchor) = cycle.iter().min() else {
        return;
    };
    let mut names: Vec<&str> = cycle
        .iter()
        .map(|&member| steps[member].name.as_str())
        .collect();
    names.push(steps[cycle[0]].name.as_str());
    ctx.error(
        steps[anchor].name_span,
        format!(
            "`after` dependencies form a cycle ({}) — steps can never start; \
             iteration is `loop` or a bounded backward route",
            names.join(" -> ")
        ),
    );
}

fn check_reachability(
    ctx: &mut Ctx<'_>,
    flow: &Flow<'_>,
    after: &[Vec<usize>],
    after_unknown: &[bool],
    routes: &[RouteEdge],
    fall_pred: &[Option<usize>],
) {
    let count = after.len();
    if count == 0 {
        return;
    }
    let mut reachable = vec![false; count];
    reachable[0] = true;
    let mut changed = true;
    while changed {
        changed = false;
        for position in 0..count {
            if reachable[position] {
                continue;
            }
            let via_after =
                !after[position].is_empty() && after[position].iter().all(|&dep| reachable[dep]);
            let via_unknown = after_unknown[position];
            let via_route = routes
                .iter()
                .any(|edge| edge.target == position && reachable[edge.source]);
            let via_fall = fall_pred[position].is_some_and(|pred| reachable[pred]);
            if via_after || via_unknown || via_route || via_fall {
                reachable[position] = true;
                changed = true;
            }
        }
    }
    for (position, step) in flow.steps.iter().enumerate() {
        if !reachable[position] {
            ctx.error(
                step.name_span,
                format!(
                    "step `{}` is unreachable: no route targets it and control never \
                     falls through to it",
                    step.name
                ),
            );
        }
    }
}

/// Every step must hand control somewhere: the final step must route
/// explicitly (a workflow may not end by running out of file), and a
/// non-final step that falls through needs a consumer — the next step
/// taking the fall-through edge, or a step depending on it via `after`.
fn check_successors(
    ctx: &mut Ctx<'_>,
    steps: &[Step],
    after: &[Vec<usize>],
    fall_pred: &[Option<usize>],
) {
    let count = steps.len();
    let mut feeds_after = vec![false; count];
    for dependencies in after {
        for &dependency in dependencies {
            feeds_after[dependency] = true;
        }
    }
    for (position, step) in steps.iter().enumerate() {
        if !falls_through(step) {
            continue;
        }
        if position + 1 == count {
            ctx.error(
                step.name_span,
                format!(
                    "the final step `{}` never routes — a workflow may not end by running \
                     out of file; route to a workflow outcome",
                    step.name
                ),
            );
        } else if fall_pred[position + 1] != Some(position) && !feeds_after[position] {
            ctx.error(
                step.name_span,
                format!(
                    "step `{}` completes into a dead end — the next step does not fall \
                     through from it, no step declares `after {}`, and it never routes; \
                     every non-terminal step needs a successor",
                    step.name, step.name
                ),
            );
        }
    }
}
