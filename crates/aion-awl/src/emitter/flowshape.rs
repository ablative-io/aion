//! The shared rev-3 flow-shape transform, run after `fold` and before the
//! planning passes of BOTH lowering backends (D-BC1 discipline: one shaping,
//! zero drift). Three rewrites:
//!
//! 1. **Regions collapse**: each `distribute`/`sequence` … `collect` pair
//!    becomes ONE synthetic host step (the opener's name, carrying the
//!    `Distribute` + `Collect` statements as lowering markers plus the close
//!    step's remaining body/outcomes), and the member steps between are
//!    extracted into a [`RegionShape`] — a nested flow planned and lowered as
//!    its own per-instance function set.
//! 2. **`visits` counters**: every step with `max … visits` reads the builtin
//!    `visits` in its outcome guards; those references rewrite to the step's
//!    language-owned counter binding (`awl_visits_<step>`), which the flow's
//!    run-once entry wrapper seeds to zero and liveness threads through the
//!    cycle.
//! 3. **Adjacent `after` normalization**: `step b after a` where `a` is the
//!    immediately preceding fall-through step is the written form of the
//!    implicit edge; dropping it lets a route-targeted step (`b`) head its
//!    own region without tripping the route-targeted+`after`-dependent
//!    refusal.

use std::collections::BTreeMap;

use crate::Span;
use crate::ast::{
    DeliveryVerb, Document, Expr, Guard, ParamDecl, Statement, Step, SubflowDecl, TypeRef,
};

use super::names::snake;

/// A shaping failure. The checker rejects every document that could produce
/// one, so reaching this from check-gated paths indicates a defect upstream;
/// it is still reported honestly, never panicked.
#[derive(Debug, Clone)]
pub(crate) struct ShapeError {
    pub(crate) span: Span,
    pub(crate) message: String,
}

/// One flow's shaped step list: regions collapsed into synthetic steps, with
/// the extracted member flows keyed by the synthetic (opener) step's name.
#[derive(Debug, Clone, Default)]
pub(crate) struct FlowSteps {
    pub(crate) steps: Vec<Step>,
    pub(crate) regions: BTreeMap<String, RegionShape>,
}

/// One extracted per-item region: the fan-out header, the member flow, and
/// the collect contract. Lowered as a per-instance nested flow whose exit
/// (reaching the close, or routing to it) returns the collected binding.
#[derive(Debug, Clone)]
pub(crate) struct RegionShape {
    /// Globally unique region id (keys the planned nested flow).
    pub(crate) id: usize,
    /// The opening step's name (the synthetic step keeps it).
    pub(crate) open_name: String,
    /// Parallel (`distribute`) or in-order (`sequence`) delivery.
    pub(crate) verb: DeliveryVerb,
    /// Per-item variable name.
    pub(crate) var: String,
    /// Collection expression the region fans out over.
    pub(crate) collection: Expr,
    /// The per-instance binding gathered by the collect.
    pub(crate) binding: String,
    /// Whether the tolerant `collect …?` form was written.
    pub(crate) tolerant: bool,
    /// The `-> name` the gathered collection binds to.
    pub(crate) collect_bind: String,
    /// The close step's name — the member flow's exit target.
    pub(crate) exit_name: String,
    /// Span of the opening statement (diagnostics).
    pub(crate) span: Span,
    /// The member flow (recursively shaped).
    pub(crate) members: FlowSteps,
}

/// One subflow, shaped: declared parameters, the single outcome, and its
/// flow. Lowered once as a nested function set; invocations call it.
#[derive(Debug, Clone)]
pub(crate) struct SubflowShape {
    pub(crate) name: String,
    pub(crate) params: Vec<ParamDecl>,
    /// The single success outcome's name (the exit route target).
    pub(crate) outcome_name: String,
    /// The outcome's payload type.
    pub(crate) outcome_ty: TypeRef,
    pub(crate) span: Span,
    pub(crate) flow: FlowSteps,
}

/// The shaped document both backends lower: the host document with its steps
/// replaced by the shaped host flow, plus the extracted nested flows.
#[derive(Debug, Clone)]
pub(crate) struct Shaped {
    pub(crate) document: Document,
    pub(crate) host_regions: BTreeMap<String, RegionShape>,
    pub(crate) subflows: Vec<SubflowShape>,
}

/// The step's language-owned visit-counter binding name.
pub(crate) fn visits_counter(step_name: &str) -> String {
    format!("awl_visits_{}", snake(step_name))
}

/// Shape a folded document: collapse regions, rewrite `visits` references,
/// and normalize adjacent `after` edges — for the host flow and every
/// subflow.
pub(crate) fn shape(document: &Document) -> Result<Shaped, ShapeError> {
    let mut ids = 0usize;
    let mut host = shape_flow(document.steps.clone(), &mut ids)?;
    let mut subflows = Vec::new();
    for decl in &document.subflows {
        subflows.push(shape_subflow(decl, &mut ids)?);
    }
    let mut shaped_document = document.clone();
    shaped_document.steps = std::mem::take(&mut host.steps);
    Ok(Shaped {
        document: shaped_document,
        host_regions: host.regions,
        subflows,
    })
}

fn shape_subflow(decl: &SubflowDecl, ids: &mut usize) -> Result<SubflowShape, ShapeError> {
    let flow = shape_flow(decl.steps.clone(), ids)?;
    Ok(SubflowShape {
        name: decl.name.clone(),
        params: decl.params.clone(),
        outcome_name: decl.outcome.name.clone(),
        outcome_ty: decl.outcome.ty.clone(),
        span: decl.name_span,
        flow,
    })
}

/// One open region while bracket-matching.
struct Open {
    opener: Step,
    members: Vec<Step>,
    inner: BTreeMap<String, RegionShape>,
}

/// Shape one flow's step list: normalize, rewrite `visits`, then collapse
/// regions by bracket nesting (a `collect` closes the nearest open region).
fn shape_flow(mut steps: Vec<Step>, ids: &mut usize) -> Result<FlowSteps, ShapeError> {
    normalize_adjacent_after(&mut steps);
    for step in &mut steps {
        rewrite_visits(step);
    }
    let mut result = FlowSteps::default();
    let mut open: Vec<Open> = Vec::new();
    for step in steps {
        if let Some(position) = distribute_position(&step) {
            let _ = position;
            open.push(Open {
                opener: step,
                members: Vec::new(),
                inner: BTreeMap::new(),
            });
            continue;
        }
        if let Some(collect) = collect_of(&step) {
            let collect = collect.clone();
            let Some(frame) = open.pop() else {
                return Err(ShapeError {
                    span: collect.span,
                    message: "`collect` closes no open region — the document did not check \
                              cleanly"
                        .to_owned(),
                });
            };
            let Some(Statement::Distribute(distribute)) = frame.opener.body.first().cloned()
            else {
                return Err(ShapeError {
                    span: frame.opener.name_span,
                    message: "region opener lost its `distribute` statement".to_owned(),
                });
            };
            let region = RegionShape {
                id: {
                    let id = *ids;
                    *ids += 1;
                    id
                },
                open_name: frame.opener.name.clone(),
                verb: distribute.verb,
                var: distribute.var.clone(),
                collection: distribute.collection.clone(),
                binding: collect.binding.clone(),
                tolerant: collect.tolerant,
                collect_bind: collect.bind.name.clone(),
                exit_name: step.name.clone(),
                span: distribute.span,
                members: FlowSteps {
                    steps: frame.members,
                    regions: frame.inner,
                },
            };
            let synthetic = synthetic_step(&frame.opener, &step, distribute.span)?;
            if let Some(parent) = open.last_mut() {
                parent.inner.insert(synthetic.name.clone(), region);
                parent.members.push(synthetic);
            } else {
                result.regions.insert(synthetic.name.clone(), region);
                result.steps.push(synthetic);
            }
            continue;
        }
        match open.last_mut() {
            Some(frame) => {
                // `after <opener>` on a member is the written form of the
                // region's entry edge; inside the member flow the opener is
                // not a step, so the edge drops here.
                let mut member = step;
                let opener = frame.opener.name.clone();
                member.after.retain(|dependency| dependency.name != opener);
                frame.members.push(member);
            }
            None => result.steps.push(step),
        }
    }
    if let Some(frame) = open.last() {
        return Err(ShapeError {
            span: frame.opener.name_span,
            message: "a per-item region never reaches its `collect` — the document did not \
                      check cleanly"
                .to_owned(),
        });
    }
    Ok(result)
}

/// Build the synthetic host step replacing a collapsed region: the opener's
/// name and dependencies, the `Distribute` + `Collect` markers, then the
/// close step's remaining body, outcomes, handler, and visits bound.
fn synthetic_step(opener: &Step, close: &Step, span: Span) -> Result<Step, ShapeError> {
    let Some(Statement::Distribute(distribute)) = opener.body.first().cloned() else {
        return Err(ShapeError {
            span,
            message: "region opener lost its `distribute` statement".to_owned(),
        });
    };
    let Some((Statement::Collect(collect), rest)) = close.body.split_first() else {
        return Err(ShapeError {
            span,
            message: "region close lost its `collect` statement".to_owned(),
        });
    };
    let mut body = Vec::with_capacity(rest.len() + 2);
    body.push(Statement::Distribute(distribute));
    body.push(Statement::Collect(collect.clone()));
    body.extend(rest.iter().cloned());
    Ok(Step {
        span: opener.span,
        lead: opener.lead.clone(),
        docs: opener.docs.clone(),
        trailing: opener.trailing.clone(),
        name: opener.name.clone(),
        name_span: opener.name_span,
        after: opener.after.clone(),
        body,
        on_failure: close.on_failure.clone(),
        outcomes: close.outcomes.clone(),
        max_visits: close.max_visits.clone(),
    })
}

/// Drop `after <previous>` when it names the immediately preceding
/// fall-through step — the written form of the implicit edge. (A previous
/// step that routes away keeps the edge, so the existing refusal still
/// names that contradiction.)
fn normalize_adjacent_after(steps: &mut [Step]) {
    for position in 1..steps.len() {
        let previous = &steps[position - 1];
        if !previous.outcomes.is_empty() || body_routes(&previous.body) {
            continue;
        }
        let previous = previous.name.clone();
        let step = &mut steps[position];
        if step.after.len() == 1 && step.after[0].name == previous {
            step.after.clear();
        }
    }
}

/// Whether a body ends in a route (mirrors `graph::body_ends_in_route`,
/// which lives beside the plan and is not visible here).
fn body_routes(body: &[Statement]) -> bool {
    match body.last() {
        Some(Statement::Route(_)) => true,
        Some(Statement::Pipe(pipe)) => matches!(pipe.end, crate::ast::PipeEnd::Route(_)),
        _ => false,
    }
}

/// Rewrite the builtin `visits` reference in a bounded step's outcome guards
/// to the step's counter binding (checker: readable only there).
fn rewrite_visits(step: &mut Step) {
    if step.max_visits.is_some() {
        let counter = visits_counter(&step.name);
        for clause in &mut step.outcomes {
            if let Guard::When { expr, .. } = &mut clause.guard {
                rewrite_visits_expr(expr, &counter);
            }
        }
    }
    for statement in &mut step.body {
        if let Statement::SubStep(sub) = statement {
            rewrite_visits(sub);
        }
    }
}

fn rewrite_visits_expr(expr: &mut Expr, counter: &str) {
    match expr {
        Expr::Ref { name, .. } => {
            if name == "visits" {
                counter.clone_into(name);
            }
        }
        Expr::List { items, .. } => {
            for item in items {
                rewrite_visits_expr(item, counter);
            }
        }
        Expr::Record { args, .. } => {
            for arg in args {
                rewrite_visits_expr(&mut arg.value, counter);
            }
        }
        Expr::Field { base, .. } | Expr::Index { base, .. } => rewrite_visits_expr(base, counter),
        Expr::Not { expr: inner, .. } => rewrite_visits_expr(inner, counter),
        Expr::Binary { left, right, .. } => {
            rewrite_visits_expr(left, counter);
            rewrite_visits_expr(right, counter);
        }
        Expr::Predicate { subject, .. } => rewrite_visits_expr(subject, counter),
        Expr::CollectionPredicate {
            collection,
            predicate,
            ..
        } => {
            rewrite_visits_expr(collection, counter);
            rewrite_visits_expr(predicate, counter);
        }
        Expr::String { .. }
        | Expr::RawString { .. }
        | Expr::Json { .. }
        | Expr::SchemaOf { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::Workflow { .. }
        | Expr::Variant { .. }
        | Expr::Accessor { .. } => {}
    }
}

/// The index of a step's top-level `distribute`/`sequence` statement.
fn distribute_position(step: &Step) -> Option<usize> {
    step.body
        .iter()
        .position(|statement| matches!(statement, Statement::Distribute(_)))
}

/// The step's opening `collect` statement, when it is a close step.
fn collect_of(step: &Step) -> Option<&crate::ast::CollectStmt> {
    match step.body.first() {
        Some(Statement::Collect(collect)) => Some(collect),
        _ => None,
    }
}
