//! Wire types of the graph projection, mirrored by hand in the ops
//! console at `features/authoring/lib/projection-types.ts` and parsed in
//! `features/authoring/lib/facade.ts` — change both together.

use serde::Serialize;

use super::super::handlers::SourceSpan;

/// One scoped projected graph: workflow or subflow steps, or sibling
/// substeps embedded under their owning parent step.
#[derive(Debug, Serialize)]
pub struct GraphProjection {
    /// One node per step, in document order.
    pub steps: Vec<ProjectionStep>,
    /// Control edges between the flow's steps.
    pub edges: Vec<ProjectionEdge>,
    /// Child-workflow contract calls made from the flow's steps.
    pub child_calls: Vec<ProjectionChildCall>,
}

/// One canvas node — exactly one step of the document.
#[derive(Debug, Serialize)]
pub struct ProjectionStep {
    /// Step name (the node id within its flow).
    pub name: String,
    /// Normalized `///` prose of the step.
    pub documentation: String,
    /// Source span of the step header.
    pub span: SourceSpan,
    /// The checker's classification (the canvas node vocabulary).
    pub kind: ProjectionStepKind,
    /// Name of the `distribute`/`sequence` step that opened the innermost
    /// per-item region containing this step, if any.
    pub region: Option<String>,
    /// The `<var> in <collection>` label of a distribute/sequence step.
    pub distribution: Option<ProjectionDistribution>,
    /// The `<binding> -> <name>` label of a collect step.
    pub collect: Option<ProjectionCollect>,
    /// The invoked subflow of a subflow-call step, with its own graph.
    pub subflow: Option<ProjectionSubflow>,
    /// Sibling `step` groups embedded in statement lists owned by this step.
    pub substeps: Vec<ProjectionSubstepGraph>,
    /// Canonical text of the step's `max N visits` bound, when written.
    pub visits: Option<String>,
    /// Whether the step carries outcome arms (its decision diamond).
    pub decision: bool,
    /// Whether the step waits durably (`wait signal` / `sleep`).
    pub waits: bool,
    /// Worker actions and child calls invoked by the step, sorted.
    pub activities: Vec<String>,
}

/// One sibling-substep graph and the statement-list scope that owns it.
#[derive(Debug, Serialize)]
pub struct ProjectionSubstepGraph {
    /// Statement-list kind containing the sibling `step` declarations.
    pub scope: ProjectionSubstepScope,
    /// Zero-based occurrence of this statement-list kind under the owner.
    pub index: usize,
    /// The sibling-local graph.
    pub graph: GraphProjection,
}

/// Statement-list location of a sibling-substep graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionSubstepScope {
    /// The owning step's main body.
    Body,
    /// The owning step's `on failure` body.
    Failure,
    /// A recursively nested `fork` body.
    Fork,
    /// A recursively nested `loop` body.
    Loop,
}

/// The checker-owned step classification, as drawn on the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStepKind {
    /// An ordinary step.
    Plain,
    /// A `distribute … in …` region opener (parallel delivery).
    Distribute,
    /// A `sequence … in …` region opener (in-order delivery).
    Sequence,
    /// A `collect` region closer.
    Collect,
    /// A step whose single statement invokes a subflow.
    SubflowCall,
    /// A body-less step with only outcome clauses: a pure decision node.
    Decision,
}

/// The label pieces of a `distribute`/`sequence` step.
#[derive(Debug, Serialize)]
pub struct ProjectionDistribution {
    /// The per-item variable.
    pub binding: String,
    /// Canonical text of the collection expression.
    pub collection: String,
}

/// The label pieces of a `collect` step.
#[derive(Debug, Serialize)]
pub struct ProjectionCollect {
    /// The per-instance binding being gathered.
    pub binding: String,
    /// Whether the tolerant `?` form was written.
    pub tolerant: bool,
    /// The gathered-collection result name.
    pub result: String,
}

/// The subflow a subflow-call step invokes.
#[derive(Debug, Serialize)]
pub struct ProjectionSubflow {
    /// The subflow's declared name.
    pub name: String,
    /// The subflow's own graph, projected with this same vocabulary.
    /// `None` only when expansion would recurse (an invocation cycle —
    /// a checker error, guarded here so projection always terminates).
    pub graph: Option<GraphProjection>,
}

/// One control edge between two steps of a flow.
#[derive(Debug, Serialize)]
pub struct ProjectionEdge {
    /// Stable edge id within the flow.
    pub id: String,
    /// Source step name.
    pub source: String,
    /// Target step name.
    pub target: String,
    /// How the edge was written.
    pub kind: ProjectionEdgeKind,
    /// Written label (`when` / `otherwise` / `route` / `failure` / `after`).
    pub label: Option<String>,
    /// Whether this route targets the same or an earlier step in document
    /// order — a cycle back-edge, drawn with its `×N` visits label.
    pub back: bool,
    /// Canonical text of the source step's `max N visits` bound, carried
    /// on back-edges as the cycle label (`×N`).
    pub visits: Option<String>,
}

/// How an edge was written in the document.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionEdgeKind {
    /// An explicit `route` (outcome arm, statement, or `on failure`).
    Route,
    /// The implicit written-order fall-through.
    FallThrough,
    /// An explicit `after` dependency.
    After,
}

/// One child-workflow contract call made from a step.
#[derive(Debug, Serialize)]
pub struct ProjectionChildCall {
    /// Stable id within the flow.
    pub id: String,
    /// The calling step.
    pub parent_step: String,
    /// The child workflow's name.
    pub name: String,
    /// Rendered `name(params…) -> returns` contract signature.
    pub signature: String,
    /// Source span of the call site.
    pub span: SourceSpan,
}
