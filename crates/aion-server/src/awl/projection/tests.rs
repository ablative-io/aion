//! Projection unit tests: the rev-3 `dev_flow` fixture is the acceptance
//! picture — five parent nodes, one nested subflow graph, one region, one
//! back edge bounded ×3.

use aion_awl::semantic;

use super::build;
use super::types::{
    GraphProjection, ProjectionEdgeKind, ProjectionStepKind, ProjectionSubstepScope,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DEV_FLOW: &str =
    include_str!("../../../../aion-awl/tests/fixtures/rev2/flow-shape/valid/dev_flow.awl");
const SUBSTEPS_TWO_STAGE: &str = include_str!(
    "../../../../aion-awl/tests/fixtures/rev2/loop-outcomes/valid/substeps_two_stage.awl"
);
const SUBSTEPS_IN_FAILURE: &str = include_str!("fixtures/substeps_in_failure.awl");
const SUBSTEPS_IN_FORK: &str = include_str!("fixtures/substeps_in_fork.awl");
const SUBSTEPS_IN_LOOP: &str = include_str!("fixtures/substeps_in_loop.awl");

fn project(source: &str) -> Result<GraphProjection, Box<dyn std::error::Error>> {
    let document = aion_awl::parse(source)?;
    let analysis = semantic::analyze(&document);
    assert!(
        analysis.diagnostics().is_empty(),
        "fixture must check clean: {:?}",
        analysis.diagnostics()
    );
    Ok(build(&document, analysis.step_kinds()))
}

#[test]
fn dev_flow_projects_exactly_five_parent_nodes_with_their_kinds() -> TestResult {
    let graph = project(DEV_FLOW)?;
    let kinds: Vec<(&str, ProjectionStepKind)> = graph
        .steps
        .iter()
        .map(|step| (step.name.as_str(), step.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("plan", ProjectionStepKind::Plain),
            ("wave", ProjectionStepKind::Distribute),
            ("build", ProjectionStepKind::SubflowCall),
            ("gather", ProjectionStepKind::Collect),
            ("fold", ProjectionStepKind::Plain),
        ]
    );
    Ok(())
}

#[test]
fn dev_flow_wave_and_gather_carry_their_label_pieces() -> TestResult {
    let graph = project(DEV_FLOW)?;
    let wave = step(&graph, "wave")?;
    let distribution = wave
        .distribution
        .as_ref()
        .ok_or("wave carries no distribution label")?;
    assert_eq!(distribution.binding, "item");
    assert_eq!(distribution.collection, "state.items");
    let gather = step(&graph, "gather")?;
    let collect = gather
        .collect
        .as_ref()
        .ok_or("gather carries no collect label")?;
    assert_eq!(collect.binding, "verdict");
    assert!(collect.tolerant, "gather collects the tolerant `?` form");
    assert_eq!(collect.result, "results");
    let fold = step(&graph, "fold")?;
    assert!(fold.decision, "fold keeps its trailing decision diamond");
    assert_eq!(fold.visits.as_deref(), Some("3"));
    Ok(())
}

#[test]
fn dev_flow_embeds_one_nested_subflow_graph() -> TestResult {
    let graph = project(DEV_FLOW)?;
    let embedded: Vec<_> = graph
        .steps
        .iter()
        .filter_map(|step| step.subflow.as_ref())
        .collect();
    assert_eq!(embedded.len(), 1, "exactly one subflow-call node");
    let subflow = embedded[0];
    assert_eq!(subflow.name, "dev_item");
    let nested = subflow.graph.as_ref().ok_or("dev_item did not expand")?;
    let kinds: Vec<(&str, ProjectionStepKind)> = nested
        .steps
        .iter()
        .map(|step| (step.name.as_str(), step.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("develop", ProjectionStepKind::Plain),
            ("review", ProjectionStepKind::Plain),
        ]
    );
    let review = step(nested, "review")?;
    assert!(review.decision, "review carries its outcome arms");
    let back: Vec<_> = nested.edges.iter().filter(|edge| edge.back).collect();
    assert_eq!(back.len(), 1, "one nested back edge");
    assert_eq!(back[0].source, "review");
    assert_eq!(back[0].target, "develop");
    assert_eq!(back[0].visits.as_deref(), Some("3"));
    Ok(())
}

#[test]
fn dev_flow_forms_one_region_owned_by_wave() -> TestResult {
    let graph = project(DEV_FLOW)?;
    let members: Vec<(&str, Option<&str>)> = graph
        .steps
        .iter()
        .map(|step| (step.name.as_str(), step.region.as_deref()))
        .collect();
    assert_eq!(
        members,
        vec![
            ("plan", None),
            ("wave", None),
            ("build", Some("wave")),
            ("gather", None),
            ("fold", None),
        ]
    );
    let regions: std::collections::BTreeSet<_> = graph
        .steps
        .iter()
        .filter_map(|step| step.region.as_deref())
        .collect();
    assert_eq!(regions.len(), 1, "exactly one region");
    Ok(())
}

#[test]
fn dev_flow_projects_one_back_edge_bounded_times_three() -> TestResult {
    let graph = project(DEV_FLOW)?;
    let back: Vec<_> = graph.edges.iter().filter(|edge| edge.back).collect();
    assert_eq!(back.len(), 1, "exactly one parent back edge");
    let edge = back[0];
    assert_eq!(edge.source, "fold");
    assert_eq!(edge.target, "wave");
    assert!(matches!(edge.kind, ProjectionEdgeKind::Route));
    assert_eq!(edge.visits.as_deref(), Some("3"));
    let forward = graph
        .edges
        .iter()
        .filter(|edge| !edge.back)
        .all(|edge| edge.visits.is_none());
    assert!(forward, "forward edges carry no visits label");
    Ok(())
}

#[test]
fn two_stage_substeps_project_as_the_parent_owned_sibling_graph() -> TestResult {
    let graph = project(SUBSTEPS_TWO_STAGE)?;
    assert_eq!(graph.steps.len(), 1, "one parent workflow step");
    let prepare = step(&graph, "prepare")?;
    let nested = substep_graph(prepare, ProjectionSubstepScope::Body, 0)?;
    let nested_steps: Vec<_> = nested
        .steps
        .iter()
        .map(|step| (step.name.as_str(), step.kind, step.region.as_deref()))
        .collect();
    assert_eq!(
        nested_steps,
        vec![
            ("fetch_batch", ProjectionStepKind::Plain, None),
            ("scrub", ProjectionStepKind::Plain, None),
        ],
        "all three authored steps project once across the two graph scopes"
    );
    assert_eq!(
        nested.edges.len(),
        1,
        "only the sibling-local route projects"
    );
    let route = &nested.edges[0];
    assert_eq!(route.source, "fetch_batch");
    assert_eq!(route.target, "scrub");
    assert!(matches!(route.kind, ProjectionEdgeKind::Route));
    assert_eq!(route.label.as_deref(), Some("when"));
    assert!(!route.back);
    assert!(
        graph.edges.is_empty(),
        "substep routes do not leak into the parent graph"
    );
    Ok(())
}

#[test]
fn failure_statement_list_projects_its_own_sibling_graph() -> TestResult {
    assert_scoped_fixture(
        SUBSTEPS_IN_FAILURE,
        "guarded",
        ProjectionSubstepScope::Failure,
        "recover_task",
        "record_failure",
    )
}

#[test]
fn fork_statement_lists_project_with_stable_occurrence_indices() -> TestResult {
    let graph = project(SUBSTEPS_IN_FORK)?;
    assert_eq!(graph.steps.len(), 1, "one parent workflow step");
    let parent = step(&graph, "forked")?;
    assert_eq!(parent.substeps.len(), 2, "two fork-local sibling graphs");
    assert_local_graph(
        substep_graph(parent, ProjectionSubstepScope::Fork, 0)?,
        "prepare",
        "verify",
    );
    assert_local_graph(
        substep_graph(parent, ProjectionSubstepScope::Fork, 1)?,
        "archive",
        "publish",
    );
    assert!(graph.edges.is_empty(), "local routes stay in their scopes");
    Ok(())
}

#[test]
fn loop_statement_list_projects_its_own_sibling_graph() -> TestResult {
    assert_scoped_fixture(
        SUBSTEPS_IN_LOOP,
        "looped",
        ProjectionSubstepScope::Loop,
        "prepare",
        "verify",
    )
}

#[test]
fn subflow_expansion_terminates_on_an_invocation_cycle() -> TestResult {
    // An invocation cycle is a checker error; the projection still guards
    // it so a graph is always produced for an in-flight buffer.
    let source = "//! Cycle guard.\n\
workflow cyclic\n\
\x20 outcome done: type String, route success\n\
\n\
subflow ping(item: String)\n\
\x20 outcome out: type String\n\
\x20 step call_pong\n\
\x20   pong(item: item) -> answer\n\
\n\
subflow pong(item: String)\n\
\x20 outcome out: type String\n\
\x20 step call_ping\n\
\x20   ping(item: item) -> answer\n\
\n\
step start\n\
\x20 ping(item: \"x\") -> result\n";
    let document = aion_awl::parse(source)?;
    let analysis = semantic::analyze(&document);
    let graph = build(&document, analysis.step_kinds());
    let start = step(&graph, "start")?;
    let first = start.subflow.as_ref().ok_or("start is a subflow call")?;
    let first_graph = first.graph.as_ref().ok_or("ping expands once")?;
    let middle = step(first_graph, "call_pong")?;
    let second = middle
        .subflow
        .as_ref()
        .ok_or("call_pong is a subflow call")?;
    let second_graph = second.graph.as_ref().ok_or("pong expands once")?;
    let deepest = step(second_graph, "call_ping")?;
    let cycled = deepest
        .subflow
        .as_ref()
        .ok_or("call_ping is a subflow call")?;
    assert!(
        cycled.graph.is_none(),
        "re-entering `ping` stops the expansion"
    );
    Ok(())
}

fn assert_scoped_fixture(
    source: &str,
    parent: &str,
    scope: ProjectionSubstepScope,
    first: &str,
    second: &str,
) -> TestResult {
    let graph = project(source)?;
    assert_eq!(graph.steps.len(), 1, "one parent workflow step");
    let parent = step(&graph, parent)?;
    assert_eq!(parent.substeps.len(), 1, "one scoped sibling graph");
    let nested = substep_graph(parent, scope, 0)?;
    assert_local_graph(nested, first, second);
    assert!(graph.edges.is_empty(), "local routes stay in their scope");
    Ok(())
}

fn assert_local_graph(nested: &GraphProjection, first: &str, second: &str) {
    let names: Vec<_> = nested.steps.iter().map(|step| step.name.as_str()).collect();
    assert_eq!(names, vec![first, second], "every authored step projects");
    assert_eq!(nested.edges.len(), 1, "one sibling-local route");
    let edge = &nested.edges[0];
    assert_eq!(edge.source, first);
    assert_eq!(edge.target, second);
    assert!(matches!(edge.kind, ProjectionEdgeKind::Route));
    assert_eq!(edge.label.as_deref(), Some("otherwise"));
}

fn substep_graph(
    step: &super::types::ProjectionStep,
    scope: ProjectionSubstepScope,
    index: usize,
) -> Result<&GraphProjection, String> {
    step.substeps
        .iter()
        .find(|candidate| candidate.scope == scope && candidate.index == index)
        .map(|candidate| &candidate.graph)
        .ok_or_else(|| format!("missing {scope:?} substep graph at index {index}"))
}

fn step<'a>(
    graph: &'a GraphProjection,
    name: &str,
) -> Result<&'a super::types::ProjectionStep, String> {
    graph
        .steps
        .iter()
        .find(|step| step.name == name)
        .ok_or_else(|| format!("missing step `{name}`"))
}
