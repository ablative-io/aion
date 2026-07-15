//! Projection builder: turns a parsed document plus the checker's semantic
//! step kinds into the canvas graph. Shape (kinds, regions) is consumed
//! from the semantic index; only labels and edge geometry are derived here.

use std::collections::{BTreeMap, BTreeSet};

use aion_awl::semantic::{StepInfo, StepKind};
use aion_awl::{
    ChildDecl, CollectStmt, DistributeStmt, Document, PipeStage, Statement, Step, SubflowDecl,
    TypeRef, expr_text,
};

use super::edges::edges;
use super::types::{
    GraphProjection, ProjectionChildCall, ProjectionCollect, ProjectionDistribution,
    ProjectionStep, ProjectionStepKind, ProjectionSubflow, ProjectionSubstepGraph,
    ProjectionSubstepScope,
};

/// Projects `document` into the canvas graph, consuming the checker's step
/// classifications (`SemanticAnalysis::step_kinds`) — the projection never
/// re-derives step shape.
pub fn build(document: &Document, step_kinds: &[StepInfo]) -> GraphProjection {
    let context = Context::new(document, step_kinds);
    let mut expanding = Vec::new();
    context.flow(&document.steps, &mut expanding)
}

/// Document-wide lookup state shared by every flow of the projection.
struct Context<'a> {
    children: BTreeMap<&'a str, &'a ChildDecl>,
    subflows: BTreeMap<&'a str, &'a SubflowDecl>,
    /// Checker classifications keyed by the step's name span.
    kinds: BTreeMap<(usize, usize), &'a StepInfo>,
}

#[derive(Default)]
struct ScopeIndices {
    fork: usize,
    looped: usize,
}

impl<'a> Context<'a> {
    fn new(document: &'a Document, step_kinds: &'a [StepInfo]) -> Self {
        Self {
            children: document
                .children
                .iter()
                .map(|child| (child.name.as_str(), child))
                .collect(),
            subflows: document
                .subflows
                .iter()
                .map(|subflow| (subflow.name.as_str(), subflow))
                .collect(),
            kinds: step_kinds
                .iter()
                .map(|info| ((info.span.start, info.span.end), info))
                .collect(),
        }
    }

    /// The checker's record for one step, when it classified it.
    fn info(&self, step: &Step) -> Option<&'a StepInfo> {
        self.kinds
            .get(&(step.name_span.start, step.name_span.end))
            .copied()
    }

    /// Projects one flow's step list. `expanding` is the stack of subflow
    /// names currently being expanded: a subflow invocation cycle is a
    /// checker error, but the projection guards it so it always terminates.
    fn flow(&self, steps: &[Step], expanding: &mut Vec<String>) -> GraphProjection {
        let steps: Vec<&Step> = steps.iter().collect();
        self.scoped_flow(&steps, expanding)
    }

    /// Projects references to one scoped sibling list. References let nested
    /// `Statement::SubStep` nodes reuse the same graph machinery without
    /// cloning the parsed AST.
    fn scoped_flow(&self, steps: &[&Step], expanding: &mut Vec<String>) -> GraphProjection {
        let projected = steps
            .iter()
            .map(|step| self.step(step, expanding))
            .collect();
        GraphProjection {
            steps: projected,
            edges: edges(steps),
            child_calls: child_calls(steps, &self.children),
        }
    }

    fn step(&self, step: &Step, expanding: &mut Vec<String>) -> ProjectionStep {
        let info = self.info(step);
        let kind = info.map_or(StepKind::Plain, |info| info.kind);
        ProjectionStep {
            name: step.name.clone(),
            documentation: documentation(&step.docs),
            span: step.span.into(),
            kind: projection_kind(kind),
            region: info.and_then(|info| info.region.clone()),
            distribution: distribute_of(step).map(|distribute| ProjectionDistribution {
                binding: distribute.var.clone(),
                collection: expr_text(&distribute.collection),
            }),
            collect: collect_of(step).map(|collect| ProjectionCollect {
                binding: collect.binding.clone(),
                tolerant: collect.tolerant,
                result: collect.bind.name.clone(),
            }),
            subflow: self.subflow_call(step, kind, expanding),
            substeps: self.substeps(step, expanding),
            visits: step
                .max_visits
                .as_ref()
                .map(|visits| expr_text(&visits.bound)),
            decision: !step.outcomes.is_empty(),
            waits: waits(step),
            activities: activities(step),
        }
    }

    /// The invoked subflow of a subflow-call step, with its own projected
    /// graph, expanded recursively (subflows nest).
    fn subflow_call(
        &self,
        step: &Step,
        kind: StepKind,
        expanding: &mut Vec<String>,
    ) -> Option<ProjectionSubflow> {
        if kind != StepKind::SubflowCall {
            return None;
        }
        let name = match step.body.as_slice() {
            [Statement::Call(call)] => call.call.name.clone(),
            _ => return None,
        };
        let declared = self.subflows.get(name.as_str()).copied();
        let graph = declared.and_then(|subflow| {
            if expanding.contains(&name) {
                return None;
            }
            expanding.push(name.clone());
            let graph = self.flow(&subflow.steps, expanding);
            expanding.pop();
            Some(graph)
        });
        Some(ProjectionSubflow { name, graph })
    }

    /// Collect sibling graphs from every statement list the checker scans:
    /// the step body, `on failure`, and recursively nested `fork`/`loop`
    /// bodies. Substeps recurse through their own projected node.
    fn substeps(&self, step: &Step, expanding: &mut Vec<String>) -> Vec<ProjectionSubstepGraph> {
        let mut groups = Vec::new();
        let mut indices = ScopeIndices::default();
        self.scan_statement_list(
            &step.body,
            ProjectionSubstepScope::Body,
            0,
            expanding,
            &mut indices,
            &mut groups,
        );
        if let Some(failure) = &step.on_failure {
            self.scan_statement_list(
                &failure.body,
                ProjectionSubstepScope::Failure,
                0,
                expanding,
                &mut indices,
                &mut groups,
            );
        }
        groups
    }

    fn scan_statement_list(
        &self,
        statements: &[Statement],
        scope: ProjectionSubstepScope,
        index: usize,
        expanding: &mut Vec<String>,
        indices: &mut ScopeIndices,
        groups: &mut Vec<ProjectionSubstepGraph>,
    ) {
        let siblings: Vec<&Step> = statements
            .iter()
            .filter_map(|statement| match statement {
                Statement::SubStep(substep) => Some(substep.as_ref()),
                _ => None,
            })
            .collect();
        if !siblings.is_empty() {
            groups.push(ProjectionSubstepGraph {
                scope,
                index,
                graph: self.scoped_flow(&siblings, expanding),
            });
        }
        for statement in statements {
            match statement {
                Statement::Fork(fork) => {
                    let index = indices.fork;
                    indices.fork += 1;
                    self.scan_statement_list(
                        &fork.body,
                        ProjectionSubstepScope::Fork,
                        index,
                        expanding,
                        indices,
                        groups,
                    );
                }
                Statement::Loop(looped) => {
                    let index = indices.looped;
                    indices.looped += 1;
                    self.scan_statement_list(
                        &looped.body,
                        ProjectionSubstepScope::Loop,
                        index,
                        expanding,
                        indices,
                        groups,
                    );
                }
                Statement::SubStep(_)
                | Statement::Call(_)
                | Statement::Spawn(_)
                | Statement::Pipe(_)
                | Statement::Wait(_)
                | Statement::Sleep(_)
                | Statement::Route(_)
                | Statement::Distribute(_)
                | Statement::Collect(_) => {}
            }
        }
    }
}

const fn projection_kind(kind: StepKind) -> ProjectionStepKind {
    match kind {
        StepKind::Plain => ProjectionStepKind::Plain,
        StepKind::Distribute => ProjectionStepKind::Distribute,
        StepKind::Sequence => ProjectionStepKind::Sequence,
        StepKind::Collect => ProjectionStepKind::Collect,
        StepKind::SubflowCall => ProjectionStepKind::SubflowCall,
        StepKind::Decision => ProjectionStepKind::Decision,
    }
}

/// The first top-level `distribute`/`sequence` statement of a step.
fn distribute_of(step: &Step) -> Option<&DistributeStmt> {
    step.body.iter().find_map(|statement| match statement {
        Statement::Distribute(distribute) => Some(distribute),
        _ => None,
    })
}

/// The first top-level `collect` statement of a step.
fn collect_of(step: &Step) -> Option<&CollectStmt> {
    step.body.iter().find_map(|statement| match statement {
        Statement::Collect(collect) => Some(collect),
        _ => None,
    })
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

fn waits(step: &Step) -> bool {
    let mut found = false;
    collect_waits(&step.body, &mut found);
    if let Some(failure) = &step.on_failure {
        collect_waits(&failure.body, &mut found);
    }
    found
}

fn collect_waits(statements: &[Statement], found: &mut bool) {
    for statement in statements {
        match statement {
            Statement::Wait(_) | Statement::Sleep(_) => *found = true,
            Statement::Fork(fork) => collect_waits(&fork.body, found),
            Statement::Loop(looped) => collect_waits(&looped.body, found),
            Statement::Call(_)
            | Statement::Spawn(_)
            | Statement::Pipe(_)
            | Statement::Route(_)
            | Statement::SubStep(_)
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
            Statement::Pipe(_)
            | Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::SubStep(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}

fn child_calls(steps: &[&Step], children: &BTreeMap<&str, &ChildDecl>) -> Vec<ProjectionChildCall> {
    let mut found = Vec::new();
    for step in steps {
        collect_child_calls(&step.name, &step.body, children, &mut found);
        if let Some(failure) = &step.on_failure {
            collect_child_calls(&step.name, &failure.body, children, &mut found);
        }
    }
    found
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
            Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::SubStep(_)
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
