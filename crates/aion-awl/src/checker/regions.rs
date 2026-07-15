//! Per-item region analysis (rev-3 flow shape): region formation over the
//! step list (bracket nesting — a bare `collect` closes the nearest open
//! region), the placement rules (`distribute`/`sequence` is its step's only
//! line, `collect` opens its step, both step-level only), the no-escape /
//! no-mid-entry routing rules, the collected binding's definite-assignment
//! duty, and step-kind classification for the semantic index.

use std::collections::{BTreeMap, BTreeSet};

use crate::Span;
use crate::ast::{DeliveryVerb, DistributeStmt, Statement, Step};
use crate::semantic::StepKind;

use super::avail::defined_names;
use super::context::{Ctx, Flow};
use super::graph::collect_route_names;

/// One matched per-item region: `open` is the `distribute`/`sequence` step,
/// `close` the `collect` step, members the steps strictly between.
pub(super) struct Region {
    /// Index of the opening `distribute`/`sequence` step.
    pub(super) open: usize,
    /// Index of the closing `collect` step.
    pub(super) close: usize,
}

impl Region {
    /// Whether a step index is strictly inside the region (a member of the
    /// per-item track; the boundary steps are not).
    pub(super) const fn inside(&self, index: usize) -> bool {
        self.open < index && index < self.close
    }
}

/// The first top-level `distribute`/`sequence` statement of a step.
pub(super) fn distribute_of(step: &Step) -> Option<&DistributeStmt> {
    step.body.iter().find_map(|statement| match statement {
        Statement::Distribute(distribute) => Some(distribute),
        _ => None,
    })
}

/// The first top-level `collect` statement of a step.
pub(super) fn collect_of(step: &Step) -> Option<&crate::ast::CollectStmt> {
    step.body.iter().find_map(|statement| match statement {
        Statement::Collect(collect) => Some(collect),
        _ => None,
    })
}

/// Enforce the placement rules: a `distribute`/`sequence` is its step's
/// only line (nothing else — no other statements, outcomes, `on failure`,
/// or `max … visits`); a `collect` opens its step (first statement); and
/// neither appears anywhere but the top level of a step body.
pub(super) fn structure(ctx: &mut Ctx<'_>, flow: &Flow<'_>) {
    for step in flow.steps {
        for (position, statement) in step.body.iter().enumerate() {
            match statement {
                Statement::Distribute(distribute) => {
                    let alone = step.body.len() == 1
                        && step.outcomes.is_empty()
                        && step.on_failure.is_none()
                        && step.max_visits.is_none();
                    if !alone {
                        ctx.error(
                            distribute.span,
                            format!(
                                "`{}` is its step's only line — step `{}` may carry \
                                 nothing else (no other statements, outcomes, `on failure`, \
                                 or `max … visits`)",
                                distribute.verb.as_word(),
                                step.name
                            ),
                        );
                    }
                }
                Statement::Collect(collect) => {
                    if position != 0 {
                        ctx.error(
                            collect.span,
                            format!(
                                "`collect` opens its step — it must be the first \
                                 statement of step `{}`",
                                step.name
                            ),
                        );
                    }
                }
                _ => {}
            }
        }
        reject_nested(ctx, &step.body, None);
        if let Some(on_failure) = &step.on_failure {
            reject_nested(ctx, &on_failure.body, Some("an `on failure` block"));
        }
    }
}

/// Walk one statement list; `context` is `None` at the top level of a step
/// body (where region statements are legal) and names the enclosing block
/// everywhere deeper, where they are rejected.
fn reject_nested(ctx: &mut Ctx<'_>, statements: &[Statement], context: Option<&str>) {
    for statement in statements {
        match statement {
            Statement::Distribute(distribute) => {
                if let Some(context) = context {
                    region_context_error(ctx, distribute.span, distribute.verb.as_word(), context);
                }
            }
            Statement::Collect(collect) => {
                if let Some(context) = context {
                    region_context_error(ctx, collect.span, "collect", context);
                }
            }
            Statement::Fork(fork) => reject_nested(ctx, &fork.body, Some("a `fork` branch")),
            Statement::Loop(looped) => reject_nested(ctx, &looped.body, Some("a `loop` body")),
            Statement::SubStep(sub) => {
                reject_nested(ctx, &sub.body, Some("a substep"));
                if let Some(on_failure) = &sub.on_failure {
                    reject_nested(ctx, &on_failure.body, Some("an `on failure` block"));
                }
            }
            _ => {}
        }
    }
}

fn region_context_error(ctx: &mut Ctx<'_>, span: Span, verb: &str, context: &str) {
    ctx.error(
        span,
        format!(
            "`{verb}` is a step-level statement — it cannot appear inside {context}; \
             give it its own top-level step"
        ),
    );
}

/// Form the regions by bracket nesting over the step list in document
/// order: a `distribute`/`sequence` step opens, a `collect` step closes the
/// NEAREST open region. A region opened inside a region therefore closes
/// inside it; interleaved or overlapping regions are unwritable.
pub(super) fn form(ctx: &mut Ctx<'_>, flow: &Flow<'_>) -> Vec<Region> {
    let mut open: Vec<usize> = Vec::new();
    let mut regions: Vec<Region> = Vec::new();
    for (index, step) in flow.steps.iter().enumerate() {
        if distribute_of(step).is_some() {
            open.push(index);
            continue;
        }
        if let Some(collect) = collect_of(step) {
            match open.pop() {
                Some(opened) => regions.push(Region {
                    open: opened,
                    close: index,
                }),
                None => {
                    ctx.error(
                        collect.span,
                        format!(
                            "this `collect` closes no region — no `distribute`/`sequence` \
                             region is open at step `{}`",
                            step.name
                        ),
                    );
                }
            }
        }
    }
    for opened in open {
        let step = &flow.steps[opened];
        if let Some(distribute) = distribute_of(step) {
            ctx.error(
                distribute.span,
                format!(
                    "`{}` opens a per-item region that never reaches a `collect` — \
                     every region closes with a `collect` step",
                    distribute.verb.as_word()
                ),
            );
        }
    }
    regions.sort_by_key(|region| (region.open, region.close));
    regions
}

/// Region-local names: everything the opening step and the member steps
/// bind (the per-item variable and every per-instance binding). These fall
/// out of scope at the region's `collect` — the per-item track is merged
/// and only the collected result crosses it.
pub(super) fn masks(flow: &Flow<'_>, regions: &[Region]) -> BTreeMap<usize, BTreeSet<String>> {
    let mut masks: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for region in regions {
        let mut locals = BTreeSet::new();
        for index in region.open..region.close {
            locals.extend(defined_names(&flow.steps[index]));
        }
        masks.entry(region.close).or_default().extend(locals);
    }
    masks
}

/// Enforce the routing and `after` rules: routes inside a region may not
/// target anything outside it (a workflow outcome included) — the only exit
/// is the region's `collect`; a route from outside may not enter the region
/// mid-track (the only entry is its `distribute`) nor target the `collect`
/// directly; `after` edges may not cross a region boundary.
pub(super) fn check_edges(ctx: &mut Ctx<'_>, flow: &Flow<'_>, regions: &[Region]) {
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for (position, step) in flow.steps.iter().enumerate() {
        index.entry(step.name.as_str()).or_insert(position);
    }
    for (source, step) in flow.steps.iter().enumerate() {
        for (name, span) in collect_route_names(step) {
            let route = RouteRef {
                name,
                span,
                target: index.get(name).copied(),
                is_outcome: flow.outcomes.contains_key(name),
            };
            for region in regions {
                check_route_edge(ctx, flow, region, source, &route);
            }
        }
        for dependency in &step.after {
            let Some(&target) = index.get(dependency.name.as_str()) else {
                continue;
            };
            for region in regions {
                check_after_edge(ctx, flow, region, source, target, dependency.span);
            }
        }
    }
}

/// One written route: its target name, span, resolved step index (when the
/// name is a step of this flow), and whether it names a flow outcome.
struct RouteRef<'n> {
    name: &'n str,
    span: Span,
    target: Option<usize>,
    is_outcome: bool,
}

fn check_route_edge(
    ctx: &mut Ctx<'_>,
    flow: &Flow<'_>,
    region: &Region,
    source: usize,
    route: &RouteRef<'_>,
) {
    let open_name = &flow.steps[region.open].name;
    let close_name = &flow.steps[region.close].name;
    if region.inside(source) {
        let stays = route
            .target
            .is_some_and(|t| region.inside(t) || t == region.close);
        if !stays && (route.target.is_some() || route.is_outcome) {
            ctx.error(
                route.span,
                format!(
                    "route to `{}` leaves the per-item region opened by step \
                     `{open_name}` — the region's only exit is its `collect` \
                     (step `{close_name}`)",
                    route.name
                ),
            );
        }
        return;
    }
    let Some(target) = route.target else {
        return;
    };
    if region.inside(target) {
        ctx.error(
            route.span,
            format!(
                "route to `{}` enters the per-item region opened by step \
                 `{open_name}` mid-track — a region is entered only through its \
                 `distribute`/`sequence` step",
                route.name
            ),
        );
    } else if target == region.close {
        ctx.error(
            route.span,
            format!(
                "step `{close_name}` is a `collect` — it is reached from inside its \
                 region, never routed to from outside (route to `{open_name}` to run \
                 the region again)"
            ),
        );
    }
}

fn check_after_edge(
    ctx: &mut Ctx<'_>,
    flow: &Flow<'_>,
    region: &Region,
    source: usize,
    target: usize,
    span: Span,
) {
    let source_inside = region.inside(source);
    let target_inside = region.inside(target);
    if source_inside == target_inside {
        return;
    }
    // A member may depend on the region's opening step: that is the
    // fall-through entry, written explicitly.
    if source_inside && target == region.open {
        return;
    }
    let open_name = &flow.steps[region.open].name;
    let close_name = &flow.steps[region.close].name;
    ctx.error(
        span,
        format!(
            "`after` may not cross the per-item region opened by step `{open_name}` — \
             the region is entered through its `distribute`/`sequence` step and left \
             through its `collect` (step `{close_name}`)"
        ),
    );
}

/// Enforce the collected binding's duties, once availability is known: the
/// binding must be produced inside the region, and definitely assigned on
/// every success path into the `collect`.
pub(super) fn check_collects(
    ctx: &mut Ctx<'_>,
    flow: &Flow<'_>,
    regions: &[Region],
    avail_in: &[BTreeSet<String>],
) {
    for region in regions {
        let step = &flow.steps[region.close];
        let Some(collect) = collect_of(step) else {
            continue;
        };
        let mut produced = BTreeSet::new();
        for index in region.open..region.close {
            produced.extend(defined_names(&flow.steps[index]));
        }
        let open_name = &flow.steps[region.open].name;
        if !produced.contains(&collect.binding) {
            ctx.error(
                collect.binding_span,
                format!(
                    "`{}` is not bound inside the region — `collect` gathers a \
                     per-instance binding produced between step `{open_name}` and \
                     this `collect`",
                    collect.binding
                ),
            );
        } else if !avail_in
            .get(region.close)
            .is_some_and(|avail| avail.contains(&collect.binding))
        {
            ctx.error(
                collect.binding_span,
                format!(
                    "`{}` is not definitely assigned on every success path through the \
                     region — every path from step `{open_name}` to this `collect` \
                     must bind it",
                    collect.binding
                ),
            );
        }
    }
}

/// Classify every step of the flow for the semantic index, so projection
/// (B3) never re-derives shape: `distribute` / `sequence` / `collect` /
/// `subflow_call` / `decision` / `plain`, each with the name of the
/// `distribute`/`sequence` step opening the innermost region containing
/// it, if any. Substeps classify recursively (they can only be plain or
/// decision — the placement rules forbid the rest) and inherit their
/// parent's region.
pub(super) fn classify(ctx: &mut Ctx<'_>, flow: &Flow<'_>, regions: &[Region]) {
    for (index, step) in flow.steps.iter().enumerate() {
        let region = containing_region(flow, regions, index);
        classify_step(ctx, step, flow.subflow.as_deref(), region);
    }
}

/// Name of the `distribute`/`sequence` step opening the innermost region
/// containing the step at `index`. Regions nest like brackets, so the
/// innermost containing region is the one opened latest.
fn containing_region<'a>(flow: &Flow<'a>, regions: &[Region], index: usize) -> Option<&'a str> {
    regions
        .iter()
        .filter(|region| region.inside(index))
        .max_by_key(|region| region.open)
        .map(|region| flow.steps[region.open].name.as_str())
}

fn classify_step(ctx: &mut Ctx<'_>, step: &Step, subflow: Option<&str>, region: Option<&str>) {
    let kind = if let Some(distribute) = distribute_of(step) {
        match distribute.verb {
            DeliveryVerb::Distribute => StepKind::Distribute,
            DeliveryVerb::Sequence => StepKind::Sequence,
        }
    } else if collect_of(step).is_some() {
        StepKind::Collect
    } else if is_subflow_call(ctx, step) {
        StepKind::SubflowCall
    } else if step.body.is_empty() && !step.outcomes.is_empty() {
        StepKind::Decision
    } else {
        StepKind::Plain
    };
    ctx.semantic
        .step_kind(step.name_span, &step.name, kind, subflow, region);
    classify_statement_lists(ctx, step, subflow, region);
}

/// Visit the same statement-list tree as cycle checking while preserving the
/// region inherited from the owning top-level step.
fn classify_statement_lists(
    ctx: &mut Ctx<'_>,
    step: &Step,
    subflow: Option<&str>,
    region: Option<&str>,
) {
    classify_list(ctx, &step.body, subflow, region);
    if let Some(on_failure) = &step.on_failure {
        classify_list(ctx, &on_failure.body, subflow, region);
    }
}

fn classify_list(
    ctx: &mut Ctx<'_>,
    statements: &[Statement],
    subflow: Option<&str>,
    region: Option<&str>,
) {
    for statement in statements {
        match statement {
            Statement::SubStep(substep) => classify_step(ctx, substep, subflow, region),
            Statement::Fork(fork) => classify_list(ctx, &fork.body, subflow, region),
            Statement::Loop(looped) => classify_list(ctx, &looped.body, subflow, region),
            _ => {}
        }
    }
}

fn is_subflow_call(ctx: &Ctx<'_>, step: &Step) -> bool {
    match step.body.as_slice() {
        [Statement::Call(call)] => ctx.subflows.contains_key(&call.call.name),
        _ => false,
    }
}
