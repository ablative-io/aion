use std::collections::{BTreeMap, BTreeSet};

use aion_awl::{ChildDecl, Document, Guard, PipeEnd, PipeStage, Statement, Step, TypeRef};
use serde::Serialize;

use super::handlers::SourceSpan;

#[derive(Debug, Serialize)]
pub struct GraphProjection {
    pub steps: Vec<ProjectionStep>,
    pub edges: Vec<ProjectionEdge>,
    pub child_calls: Vec<ProjectionChildCall>,
}

#[derive(Debug, Serialize)]
pub struct ProjectionStep {
    pub name: String,
    pub documentation: String,
    pub span: SourceSpan,
    pub markers: StepMarkers,
    pub activities: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct StepMarkers {
    pub looped: bool,
    pub forked: bool,
    pub waits: bool,
}

#[derive(Debug, Serialize)]
pub struct ProjectionEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub kind: ProjectionEdgeKind,
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionEdgeKind {
    Route,
    FallThrough,
    After,
}

#[derive(Debug, Serialize)]
pub struct ProjectionChildCall {
    pub id: String,
    pub parent_step: String,
    pub name: String,
    pub signature: String,
    pub span: SourceSpan,
}

pub fn build(document: &Document) -> GraphProjection {
    let step_names: BTreeSet<_> = document
        .steps
        .iter()
        .map(|step| step.name.as_str())
        .collect();
    let children: BTreeMap<_, _> = document
        .children
        .iter()
        .map(|child| (child.name.as_str(), child))
        .collect();
    let steps = document
        .steps
        .iter()
        .map(|step| ProjectionStep {
            name: step.name.clone(),
            documentation: documentation(&step.docs),
            span: step.span.into(),
            markers: markers(step),
            activities: activities(step),
        })
        .collect();

    let mut edges = Vec::new();
    let mut route_targets = BTreeSet::new();
    for step in &document.steps {
        collect_step_routes(step, &step_names, &mut route_targets, &mut edges);
    }
    for step in &document.steps {
        for dependency in &step.after {
            if step_names.contains(dependency.name.as_str()) {
                edges.push(ProjectionEdge {
                    id: format!("after:{}:{}", dependency.name, step.name),
                    source: dependency.name.clone(),
                    target: step.name.clone(),
                    kind: ProjectionEdgeKind::After,
                    label: Some("after".to_owned()),
                });
            }
        }
    }
    for pair in document.steps.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if current.after.is_empty()
            && !route_targets.contains(current.name.as_str())
            && falls_through(previous)
        {
            edges.push(ProjectionEdge {
                id: format!("fall:{}:{}", previous.name, current.name),
                source: previous.name.clone(),
                target: current.name.clone(),
                kind: ProjectionEdgeKind::FallThrough,
                label: None,
            });
        }
    }

    let mut child_calls = Vec::new();
    for step in &document.steps {
        collect_child_calls(&step.name, &step.body, &children, &mut child_calls);
        if let Some(failure) = &step.on_failure {
            collect_child_calls(&step.name, &failure.body, &children, &mut child_calls);
        }
    }
    GraphProjection {
        steps,
        edges,
        child_calls,
    }
}

fn documentation(lines: &[aion_awl::DocLine]) -> String {
    lines
        .iter()
        .map(|line| line.text.strip_prefix(' ').unwrap_or(&line.text).trim_end())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

fn markers(step: &Step) -> StepMarkers {
    let mut markers = StepMarkers::default();
    collect_markers(&step.body, &mut markers);
    if let Some(failure) = &step.on_failure {
        collect_markers(&failure.body, &mut markers);
    }
    markers
}

fn collect_markers(statements: &[Statement], markers: &mut StepMarkers) {
    for statement in statements {
        match statement {
            Statement::Wait(_) | Statement::Sleep(_) => markers.waits = true,
            Statement::Fork(fork) => {
                markers.forked = true;
                collect_markers(&fork.body, markers);
            }
            Statement::Loop(looped) => {
                markers.looped = true;
                collect_markers(&looped.body, markers);
            }
            Statement::SubStep(step) => collect_markers(&step.body, markers),
            Statement::Call(_)
            | Statement::Spawn(_)
            | Statement::Pipe(_)
            | Statement::Route(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}

fn activities(step: &Step) -> Vec<String> {
    let mut names = BTreeSet::new();
    collect_activities(&step.body, &mut names);
    if let Some(failure) = &step.on_failure {
        collect_activities(&failure.body, &mut names);
    }
    names.into_iter().collect()
}

fn collect_activities(statements: &[Statement], names: &mut BTreeSet<String>) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                names.insert(call.call.name.clone());
            }
            Statement::Spawn(spawn) => {
                names.insert(spawn.call.name.clone());
            }
            Statement::Fork(fork) => collect_activities(&fork.body, names),
            Statement::Loop(looped) => collect_activities(&looped.body, names),
            Statement::SubStep(step) => collect_activities(&step.body, names),
            Statement::Pipe(_)
            | Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
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
    edges.push(ProjectionEdge {
        id: format!("route:{source}:{target}:{label}:{index}"),
        source: source.to_owned(),
        target: target.to_owned(),
        kind: ProjectionEdgeKind::Route,
        label: Some(label.to_owned()),
    });
}

fn falls_through(step: &Step) -> bool {
    step.outcomes.is_empty()
        && !matches!(step.body.last(), Some(Statement::Route(_)))
        && !matches!(
            step.body.last(),
            Some(Statement::Pipe(pipe)) if matches!(pipe.end, PipeEnd::Route(_))
        )
}

fn collect_child_calls(
    parent_step: &str,
    statements: &[Statement],
    children: &BTreeMap<&str, &ChildDecl>,
    found: &mut Vec<ProjectionChildCall>,
) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                add_child_call(
                    parent_step,
                    &call.call.name,
                    call.call.span,
                    children,
                    found,
                );
            }
            Statement::Spawn(spawn) => {
                add_child_call(
                    parent_step,
                    &spawn.call.name,
                    spawn.call.span,
                    children,
                    found,
                );
            }
            Statement::Pipe(pipe) => {
                for stage in &pipe.stages {
                    if let PipeStage::Action { span, name } = stage {
                        add_child_call(parent_step, name, *span, children, found);
                    }
                }
            }
            Statement::Fork(fork) => {
                collect_child_calls(parent_step, &fork.body, children, found);
            }
            Statement::Loop(looped) => {
                collect_child_calls(parent_step, &looped.body, children, found);
            }
            Statement::SubStep(step) => {
                collect_child_calls(parent_step, &step.body, children, found);
            }
            Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}

fn add_child_call(
    parent_step: &str,
    name: &str,
    span: aion_awl::Span,
    children: &BTreeMap<&str, &ChildDecl>,
    found: &mut Vec<ProjectionChildCall>,
) {
    let Some(child) = children.get(name) else {
        return;
    };
    found.push(ProjectionChildCall {
        id: format!("child:{parent_step}:{}:{}", span.start, child.name),
        parent_step: parent_step.to_owned(),
        name: child.name.clone(),
        signature: child_signature(child),
        span: span.into(),
    });
}

fn child_signature(child: &ChildDecl) -> String {
    let parameters = child
        .params
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name, type_text(&parameter.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}({parameters}) -> {}",
        child.name,
        type_text(&child.returns)
    )
}

fn type_text(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => name.clone(),
        TypeRef::List { inner, .. } => format!("[{}]", type_text(inner)),
        TypeRef::Optional { inner, .. } => format!("{}?", type_text(inner)),
    }
}
