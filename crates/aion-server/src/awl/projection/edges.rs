//! Control-edge projection shared by top-level, subflow, and sibling-substep
//! graphs. Its sibling rules mirror the checker in `checker/graph.rs` and
//! `checker/cycles.rs`: local routes first, then `after` dependencies, then
//! eligible written-order fall-throughs.

use std::collections::{BTreeMap, BTreeSet};

use aion_awl::{Guard, PipeEnd, Statement, Step, expr_text};

use super::types::{ProjectionEdge, ProjectionEdgeKind};

/// Every control edge of one scoped step list. Backward routes (document
/// order) are marked as cycle back-edges carrying the source step's visits
/// bound.
pub(super) fn edges(steps: &[&Step]) -> Vec<ProjectionEdge> {
    let step_names: BTreeSet<_> = steps.iter().map(|step| step.name.as_str()).collect();
    let positions: BTreeMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.name.as_str(), index))
        .collect();
    let mut edges = Vec::new();
    let mut route_targets = BTreeSet::new();
    for step in steps {
        collect_step_routes(step, &step_names, &mut route_targets, &mut edges);
    }
    for edge in &mut edges {
        let (Some(&source), Some(&target)) = (
            positions.get(edge.source.as_str()),
            positions.get(edge.target.as_str()),
        ) else {
            continue;
        };
        if target <= source {
            edge.back = true;
            edge.visits = steps[source]
                .max_visits
                .as_ref()
                .map(|visits| expr_text(&visits.bound));
        }
    }
    for step in steps {
        for dependency in &step.after {
            if step_names.contains(dependency.name.as_str()) {
                edges.push(edge(
                    format!("after:{}:{}", dependency.name, step.name),
                    &dependency.name,
                    &step.name,
                    ProjectionEdgeKind::After,
                    Some("after".to_owned()),
                ));
            }
        }
    }
    for pair in steps.windows(2) {
        let previous = pair[0];
        let current = pair[1];
        if current.after.is_empty()
            && !route_targets.contains(current.name.as_str())
            && falls_through(previous)
        {
            edges.push(edge(
                format!("fall:{}:{}", previous.name, current.name),
                &previous.name,
                &current.name,
                ProjectionEdgeKind::FallThrough,
                None,
            ));
        }
    }
    edges
}

fn edge(
    id: String,
    source: &str,
    target: &str,
    kind: ProjectionEdgeKind,
    label: Option<String>,
) -> ProjectionEdge {
    ProjectionEdge {
        id,
        source: source.to_owned(),
        target: target.to_owned(),
        kind,
        label,
        back: false,
        visits: None,
    }
}

fn collect_step_routes(
    step: &Step,
    step_names: &BTreeSet<&str>,
    targets: &mut BTreeSet<String>,
    edges: &mut Vec<ProjectionEdge>,
) {
    collect_statement_routes(&step.name, &step.body, "route", step_names, targets, edges);
    if let Some(failure) = &step.on_failure {
        collect_statement_routes(
            &step.name,
            &failure.body,
            "failure",
            step_names,
            targets,
            edges,
        );
    }
    for (index, outcome) in step.outcomes.iter().enumerate() {
        let label = match outcome.guard {
            Guard::When { .. } => "when",
            Guard::Otherwise { .. } => "otherwise",
        };
        push_route(
            &step.name,
            &outcome.route.name,
            label,
            index,
            step_names,
            targets,
            edges,
        );
    }
}

fn collect_statement_routes(
    source: &str,
    statements: &[Statement],
    label: &str,
    step_names: &BTreeSet<&str>,
    targets: &mut BTreeSet<String>,
    edges: &mut Vec<ProjectionEdge>,
) {
    for (index, statement) in statements.iter().enumerate() {
        match statement {
            Statement::Route(route) => push_route(
                source,
                &route.target.name,
                label,
                index,
                step_names,
                targets,
                edges,
            ),
            Statement::Pipe(pipe) => {
                if let PipeEnd::Route(target) = &pipe.end {
                    push_route(
                        source,
                        &target.name,
                        label,
                        index,
                        step_names,
                        targets,
                        edges,
                    );
                }
            }
            Statement::Fork(fork) => {
                collect_statement_routes(source, &fork.body, label, step_names, targets, edges);
            }
            Statement::Loop(looped) => {
                collect_statement_routes(source, &looped.body, label, step_names, targets, edges);
            }
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

fn push_route(
    source: &str,
    target: &str,
    label: &str,
    index: usize,
    step_names: &BTreeSet<&str>,
    targets: &mut BTreeSet<String>,
    edges: &mut Vec<ProjectionEdge>,
) {
    if !step_names.contains(target) {
        return;
    }
    targets.insert(target.to_owned());
    edges.push(edge(
        format!("route:{source}:{target}:{label}:{index}"),
        source,
        target,
        ProjectionEdgeKind::Route,
        Some(label.to_owned()),
    ));
}

fn falls_through(step: &Step) -> bool {
    step.outcomes.is_empty()
        && !matches!(step.body.last(), Some(Statement::Route(_)))
        && !matches!(
            step.body.last(),
            Some(Statement::Pipe(pipe)) if matches!(pipe.end, PipeEnd::Route(_))
        )
}
