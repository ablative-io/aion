//! Control-flow planning for the Gleam lowering.
//!
//! The rev-2 surface is a DAG of steps with conditional routes; generated
//! Gleam is structured code. The mapping: dependency-connected steps group
//! into *regions* (one entry each — the workflow start or a route target),
//! each region lowers to one Gleam function running its steps in
//! topological layers, and every `route <step>` is a tail call to the target
//! region's function. Substeps lower to sibling functions inside their
//! parent's region. Bindings thread between functions as parameters computed
//! by a liveness fixed-point over the call graph (backward routes make it
//! cyclic, so this is iterate-to-stable, not a single pass).
//!
//! Shapes the stopgap refuses (recorded as decision-log mapping limits in
//! AWL-2-BUILD-PLAN.md): a route-targeted step with `after` dependencies, a
//! routing step with `after`-dependents, regions with two entries, substep
//! parents that fall through, substeps outside a trailing block at nesting
//! depth one, and a `route` inside a `loop` body (the generated loop
//! function has no early-exit channel).

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Expr, PipeEnd, Statement, Step};

use super::context::Emitter;
use super::error::EmitError;

/// One dependency-connected group of steps, lowered as one Gleam function.
pub(crate) struct Region {
    /// Entry step index (the function is named for it).
    pub(crate) entry: usize,
    /// Topological layers of member step indexes, written order within a
    /// layer.
    pub(crate) layers: Vec<Vec<usize>>,
}

/// A function node in the liveness graph.
#[derive(Debug, Default)]
pub(crate) struct Node {
    pub(crate) refs: BTreeSet<String>,
    pub(crate) defs: BTreeSet<String>,
    pub(crate) callees: BTreeSet<usize>,
}

/// The lowering plan: regions, substep chains, and per-function parameters.
pub(crate) struct Plan {
    pub(crate) regions: Vec<Region>,
    /// Step index → region index of which it is the entry.
    pub(crate) entry_region: BTreeMap<usize, usize>,
    /// Region index → liveness node.
    pub(crate) region_node: Vec<usize>,
    /// (parent step index, substep position) → liveness node.
    pub(crate) sub_node: BTreeMap<(usize, usize), usize>,
    /// Per-node parameter lists (sorted, deterministic).
    pub(crate) params: Vec<Vec<String>>,
}

impl Plan {
    pub(crate) fn region_params(&self, region: usize) -> &[String] {
        &self.params[self.region_node[region]]
    }

    pub(crate) fn sub_params(&self, step: usize, sub: usize) -> &[String] {
        &self.params[self.sub_node[&(step, sub)]]
    }

    /// The region a route-targeted step heads, when it heads one.
    pub(crate) fn region_of_entry(&self, step: usize) -> Option<usize> {
        self.entry_region.get(&step).copied()
    }
}

/// Where a step body's trailing substep block starts, validating the shape.
pub(crate) fn substep_split(step: &Step) -> Result<usize, EmitError> {
    let first = step
        .body
        .iter()
        .position(|statement| matches!(statement, Statement::SubStep(_)))
        .unwrap_or(step.body.len());
    for statement in &step.body[first..] {
        let Statement::SubStep(sub) = statement else {
            return Err(EmitError::new(
                step.name_span,
                format!(
                    "step `{}` mixes statements after its substeps — the Gleam stopgap \
                     lowers substeps only as a trailing block",
                    step.name
                ),
            ));
        };
        if sub
            .body
            .iter()
            .any(|inner| matches!(inner, Statement::SubStep(_)))
        {
            return Err(EmitError::new(
                sub.name_span,
                format!(
                    "substep `{}` nests further substeps — the Gleam stopgap lowers one \
                     level of substeps",
                    sub.name
                ),
            ));
        }
    }
    if first < step.body.len() && step.outcomes.is_empty() {
        return Err(EmitError::new(
            step.name_span,
            format!(
                "step `{}` contains substeps but no outcome clauses — the Gleam stopgap \
                 needs the parent's outcomes as the substep chain's boundary",
                step.name
            ),
        ));
    }
    Ok(first)
}

/// Whether a step can complete and hand control onward (mirrors the
/// checker's rule).
pub(crate) fn falls_through(step: &Step) -> bool {
    step.outcomes.is_empty() && !body_ends_in_route(&step.body)
}

pub(crate) fn body_ends_in_route(body: &[Statement]) -> bool {
    match body.last() {
        Some(Statement::Route(_)) => true,
        Some(Statement::Pipe(pipe)) => matches!(pipe.end, PipeEnd::Route(_)),
        _ => false,
    }
}

/// The resolved edge sets of the step graph.
struct Edges {
    index: BTreeMap<String, usize>,
    after: Vec<Vec<usize>>,
    fall_pred: Vec<Option<usize>>,
    route_targeted: Vec<bool>,
    step_routes: Vec<Vec<usize>>,
}

/// Build the lowering plan for the document's steps.
pub(crate) fn plan(emitter: &Emitter<'_>) -> Result<Plan, EmitError> {
    let steps = &emitter.document.steps;
    let edges = build_edges(steps)?;
    check_refusals(steps, &edges)?;
    let (regions, entry_region) = build_regions(steps, &edges)?;
    super::liveness::build_params(emitter, steps, regions, entry_region, &edges.index)
}

fn build_edges(steps: &[Step]) -> Result<Edges, EmitError> {
    let count = steps.len();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for (position, step) in steps.iter().enumerate() {
        if index.insert(step.name.clone(), position).is_some() {
            return Err(EmitError::new(
                step.name_span,
                format!("duplicate step `{}`", step.name),
            ));
        }
    }
    let mut after: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (position, step) in steps.iter().enumerate() {
        for dependency in &step.after {
            let Some(&target) = index.get(dependency.name.as_str()) else {
                return Err(EmitError::new(
                    dependency.span,
                    format!("`after {}` names no step", dependency.name),
                ));
            };
            after[position].push(target);
        }
    }
    // Step-level route edges (substep routes resolve within their parent).
    let mut route_targeted = vec![false; count];
    let mut step_routes: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (position, step) in steps.iter().enumerate() {
        for name in step_route_names(step) {
            if let Some(&target) = index.get(name) {
                route_targeted[target] = true;
                step_routes[position].push(target);
            }
        }
    }
    let mut fall_pred: Vec<Option<usize>> = vec![None; count];
    for position in 1..count {
        if after[position].is_empty()
            && !route_targeted[position]
            && falls_through(&steps[position - 1])
        {
            fall_pred[position] = Some(position - 1);
        }
    }
    Ok(Edges {
        index,
        after,
        fall_pred,
        route_targeted,
        step_routes,
    })
}

/// Refuse shapes the sequential lowering cannot honor.
fn check_refusals(steps: &[Step], edges: &Edges) -> Result<(), EmitError> {
    for (position, step) in steps.iter().enumerate() {
        if edges.route_targeted[position] && !edges.after[position].is_empty() {
            return Err(EmitError::new(
                step.name_span,
                format!(
                    "step `{}` is both route-targeted and `after`-dependent — the Gleam \
                     stopgap cannot express that join (AWL-BC #240 carries it)",
                    step.name
                ),
            ));
        }
        if !falls_through(step) {
            for (dependent, dependencies) in edges.after.iter().enumerate() {
                if dependencies.contains(&position) {
                    return Err(EmitError::new(
                        steps[dependent].name_span,
                        format!(
                            "step `{}` declares `after {}`, but `{}` routes away instead of \
                             completing into its dependents — the Gleam stopgap cannot run \
                             both continuations (AWL-BC #240 carries it)",
                            steps[dependent].name, step.name, step.name
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Path-compressing union-find lookup.
fn find(parent: &mut [usize], node: usize) -> usize {
    let mut root = node;
    while parent[root] != root {
        root = parent[root];
    }
    let mut cursor = node;
    while parent[cursor] != root {
        let next = parent[cursor];
        parent[cursor] = root;
        cursor = next;
    }
    root
}

/// Union-find over dependency edges (after + fall-through) → regions with
/// one validated entry each, layered topologically.
fn build_regions(
    steps: &[Step],
    edges: &Edges,
) -> Result<(Vec<Region>, BTreeMap<usize, usize>), EmitError> {
    let count = steps.len();
    let mut parent: Vec<usize> = (0..count).collect();
    for (position, dependencies) in edges.after.iter().enumerate() {
        for &dependency in dependencies {
            let a = find(&mut parent, position);
            let b = find(&mut parent, dependency);
            parent[a.max(b)] = a.min(b);
        }
    }
    for (position, pred) in edges.fall_pred.iter().enumerate() {
        if let Some(&source) = pred.as_ref() {
            let a = find(&mut parent, position);
            let b = find(&mut parent, source);
            parent[a.max(b)] = a.min(b);
        }
    }
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for position in 0..count {
        let root = find(&mut parent, position);
        members.entry(root).or_default().push(position);
    }

    let mut regions = Vec::new();
    let mut entry_region = BTreeMap::new();
    for group in members.values() {
        let roots: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&member| edges.after[member].is_empty() && edges.fall_pred[member].is_none())
            .collect();
        let [entry] = roots.as_slice() else {
            let step = &steps[group[0]];
            return Err(EmitError::new(
                step.name_span,
                format!(
                    "the dependency group containing step `{}` has {} entry steps — the \
                     Gleam stopgap lowers one entry per group",
                    step.name,
                    roots.len()
                ),
            ));
        };
        let layers = layer(group, &edges.after, &edges.fall_pred);
        for (position, layer_members) in layers.iter().enumerate() {
            let last = position + 1 == layers.len();
            for &member in layer_members {
                if !falls_through(&steps[member]) && (!last || layer_members.len() > 1) {
                    return Err(EmitError::new(
                        steps[member].name_span,
                        format!(
                            "step `{}` routes away while parallel or upstream work in its \
                             dependency group is outstanding — the Gleam stopgap cannot \
                             express that (AWL-BC #240 carries it)",
                            steps[member].name
                        ),
                    ));
                }
            }
        }
        entry_region.insert(*entry, regions.len());
        regions.push(Region {
            entry: *entry,
            layers,
        });
    }

    if count > 0 && !entry_region.contains_key(&0) {
        return Err(EmitError::new(
            steps[0].name_span,
            format!(
                "the first step `{}` is not its dependency group's entry — the workflow \
                 start must head a group",
                steps[0].name
            ),
        ));
    }
    // Every route target must head a region.
    for (position, targets) in edges.step_routes.iter().enumerate() {
        for &target in targets {
            if !entry_region.contains_key(&target) {
                return Err(EmitError::new(
                    steps[position].name_span,
                    format!(
                        "step `{}` routes to `{}`, which sits mid-chain in a dependency \
                         group — the Gleam stopgap routes only to group entries",
                        steps[position].name, steps[target].name
                    ),
                ));
            }
        }
    }
    Ok((regions, entry_region))
}

/// Kahn layering of one region's members over dependency edges.
fn layer(group: &[usize], after: &[Vec<usize>], fall_pred: &[Option<usize>]) -> Vec<Vec<usize>> {
    let mut level: BTreeMap<usize, usize> = BTreeMap::new();
    // Members arrive sorted by position; dependencies always point backward
    // in written order for fall-through, and `after` cycles are a checker
    // error, so two passes settle levels for well-formed documents.
    for _ in 0..group.len() {
        for &member in group {
            let mut depth = 0;
            for &dependency in &after[member] {
                depth = depth.max(level.get(&dependency).copied().unwrap_or(0) + 1);
            }
            if let Some(&source) = fall_pred[member].as_ref() {
                depth = depth.max(level.get(&source).copied().unwrap_or(0) + 1);
            }
            level.insert(member, depth);
        }
    }
    let mut layers: Vec<Vec<usize>> = Vec::new();
    for &member in group {
        let depth = level.get(&member).copied().unwrap_or(0);
        while layers.len() <= depth {
            layers.push(Vec::new());
        }
        layers[depth].push(member);
    }
    layers.retain(|layer_members| !layer_members.is_empty());
    layers
}

/// Route target names written on a step's own surface (bodies, `on failure`,
/// outcome clauses) — substeps excluded, their routes resolve within the
/// parent.
fn step_route_names(step: &Step) -> Vec<&str> {
    let mut found = Vec::new();
    route_names_in(&step.body, &mut found);
    if let Some(on_failure) = &step.on_failure {
        route_names_in(&on_failure.body, &mut found);
    }
    for clause in &step.outcomes {
        found.push(clause.route.name.as_str());
    }
    found
}

fn route_names_in<'a>(statements: &'a [Statement], found: &mut Vec<&'a str>) {
    for statement in statements {
        match statement {
            Statement::Pipe(pipe) => {
                if let PipeEnd::Route(target) = &pipe.end {
                    found.push(target.name.as_str());
                }
            }
            Statement::Route(route) => found.push(route.target.name.as_str()),
            Statement::Fork(fork) => route_names_in(&fork.body, found),
            Statement::Loop(looped) => route_names_in(&looped.body, found),
            // Region statements carry no routes (their step is the node).
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

/// Names an expression references.
pub(crate) fn expr_refs(expr: &Expr, refs: &mut BTreeSet<String>) {
    match expr {
        Expr::String { .. }
        | Expr::RawString { .. }
        | Expr::Json { .. }
        | Expr::SchemaOf { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::Variant { .. }
        | Expr::Workflow { .. }
        | Expr::Accessor { .. } => {}
        Expr::List { items, .. } => {
            for item in items {
                expr_refs(item, refs);
            }
        }
        Expr::Ref { name, .. } => {
            refs.insert(name.clone());
        }
        Expr::Record { args, .. } => {
            for arg in args {
                expr_refs(&arg.value, refs);
            }
        }
        Expr::Field { base, .. } | Expr::Index { base, .. } => expr_refs(base, refs),
        Expr::Not { expr: inner, .. } => expr_refs(inner, refs),
        Expr::Binary { left, right, .. } => {
            expr_refs(left, refs);
            expr_refs(right, refs);
        }
        Expr::Predicate { subject, .. } => expr_refs(subject, refs),
        Expr::CollectionPredicate {
            collection,
            predicate,
            ..
        } => {
            expr_refs(collection, refs);
            expr_refs(predicate, refs);
        }
    }
}
