//! The `lower` entry point: run the emitter's shared planning passes (D-BC1),
//! assemble the module skeleton, fill region bodies, and return the
//! `MirModule`. Deferred shapes surface as `LowerError::Unsupported` — a
//! BC-2-scope marker distinct from a D-BC3 parity refusal.

use std::fmt;
use std::mem;
use std::path::Path;

use crate::ast::Document;
use crate::emitter::{prepare, snake};

use super::super::unit::MirModule;
use super::build;
use super::ctx::Ctx;
use super::flow;

/// A lowering failure. `Unsupported` marks a shape this BC-2 increment does not
/// yet lower (NOT a reference refusal); `Planning` wraps an emitter planning
/// error (a genuine refusal or a document that did not check cleanly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerError {
    Unsupported { shape: String, span: crate::Span },
    Message { message: String, span: crate::Span },
    Planning { message: String },
}

impl LowerError {
    pub(super) fn unsupported(shape: &str, span: crate::Span) -> Self {
        Self::Unsupported {
            shape: shape.to_owned(),
            span,
        }
    }

    pub(super) fn new(span: crate::Span, message: impl Into<String>) -> Self {
        Self::Message {
            message: message.into(),
            span,
        }
    }

    /// Preserve the retired refusal's source anchor while narrowing its shape.
    pub(super) fn reanchor_unsupported(self, anchor: crate::Span) -> Self {
        match self {
            Self::Unsupported { shape, .. } => Self::Unsupported {
                shape,
                span: anchor,
            },
            other => other,
        }
    }
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { shape, span } => {
                write!(f, "BC-2 does not yet lower {shape} (line {})", span.line)
            }
            Self::Message { message, span } => write!(f, "{message} (line {})", span.line),
            Self::Planning { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for LowerError {}

/// The first rev-3 construct the direct path does not yet mirror (the B4
/// staging gate: constructs retire from this list as their MIR lowering
/// lands).
fn flow_shape_gap(document: &Document) -> Option<(crate::Span, &'static str)> {
    use crate::ast::{RoutePayload, RouteTarget, Statement, Step};
    fn route_gap(target: &RouteTarget) -> Option<(crate::Span, &'static str)> {
        match &target.payload {
            Some(RoutePayload::Value(value)) => Some((
                crate::spanned::Spanned::span(value),
                "value route payloads (`route out(<value>)`)",
            )),
            _ => None,
        }
    }
    fn statements_gap(statements: &[Statement]) -> Option<(crate::Span, &'static str)> {
        for statement in statements {
            let found = match statement {
                Statement::Distribute(distribute) => {
                    Some((distribute.span, "`distribute`/`sequence` regions"))
                }
                Statement::Collect(collect) => Some((collect.span, "`collect` steps")),
                Statement::Route(route) => route_gap(&route.target),
                Statement::Pipe(pipe) => match &pipe.end {
                    crate::ast::PipeEnd::Route(target) => route_gap(target),
                    crate::ast::PipeEnd::Bind(_) => None,
                },
                Statement::Fork(fork) => statements_gap(&fork.body),
                Statement::Loop(looped) => statements_gap(&looped.body),
                Statement::SubStep(sub) => step_gap(sub),
                Statement::Call(_)
                | Statement::Spawn(_)
                | Statement::Wait(_)
                | Statement::Sleep(_) => None,
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }
    fn step_gap(step: &Step) -> Option<(crate::Span, &'static str)> {
        if let Some(max_visits) = &step.max_visits {
            return Some((max_visits.span, "the `max … visits` step attribute"));
        }
        if let Some(found) = statements_gap(&step.body) {
            return Some(found);
        }
        if let Some(on_failure) = &step.on_failure
            && let Some(found) = statements_gap(&on_failure.body)
        {
            return Some(found);
        }
        for clause in &step.outcomes {
            if let Some(found) = route_gap(&clause.route) {
                return Some(found);
            }
        }
        None
    }
    if let Some(subflow) = document.subflows.first() {
        return Some((subflow.name_span, "`subflow` declarations"));
    }
    document.steps.iter().find_map(step_gap)
}

/// Lower a checked document to its MIR module.
///
/// # Errors
///
/// Returns [`LowerError::Planning`] for a document the shared passes refuse or
/// that did not check cleanly, and [`LowerError::Unsupported`] for a shape this
/// BC-2 increment does not yet cover.
pub fn lower(document: &Document, root: Option<&Path>) -> Result<MirModule, LowerError> {
    // Lowering is defined only for documents that check cleanly: fold-time
    // const substitution is name-based and relies on the checker's
    // invariants (no shadowed consts, no input/signal collisions).
    let diagnostics = match root {
        Some(root) => crate::checker::check_in(document, root),
        None => crate::checker::check(document),
    };
    if let Some(first) = diagnostics.first() {
        return Err(LowerError::Message {
            message: format!("document does not check cleanly: {}", first.message),
            span: first.span,
        });
    }
    // The one rev-3 refusal both backends keep: substep `after` would drop
    // on the floor — refuse honestly, with a span, before planning.
    if let Some((span, what)) = crate::unlowered::first_unlowered(document) {
        return Err(LowerError::Unsupported {
            shape: format!("{what} — a later landing carries them"),
            span,
        });
    }
    // TEMPORARY B4 staging gate: the direct path grows the rev-3 lowering
    // piecewise behind the emitter; anything not yet mirrored refuses
    // honestly here (removed as each construct lands).
    if let Some((span, what)) = flow_shape_gap(document) {
        return Err(LowerError::Unsupported {
            shape: format!("{what} — the rev-3 flow shape is not yet lowered (B4)"),
            span,
        });
    }
    // Fold the B1 ergonomics vocabulary down to plain literals, then shape
    // the rev-3 flow constructs (regions, subflows, visit counters) into
    // the planned form both backends lower.
    let shaped =
        crate::emitter::shape_document(document, root).map_err(|error| LowerError::Planning {
            message: error.to_string(),
        })?;
    let (emitter, plans) = prepare(&shaped, root).map_err(|error| LowerError::Planning {
        message: error.to_string(),
    })?;
    let plan = plans.host;
    let document = emitter.document;
    let module_name = snake(&document.name);
    let source = format!("{module_name}.awl");

    let mut ctx = Ctx::new(&emitter, &plan, module_name.clone());
    let mut skeleton = build::skeleton(&mut ctx)?;
    ctx.set_predicate_start(skeleton.plan.predicate_start);
    let mut slots = super::slots::Slots {
        loops: super::loops::LoopSlots::new(skeleton.plan.loops.clone()),
        forks: super::slots::ForkSlots::new(skeleton.plan.forks.clone()),
        waits: super::slots::WaitSlots::new(skeleton.plan.waits.clone()),
    };
    flow::lower_regions(
        &mut ctx,
        &skeleton.plan,
        &mut skeleton.functions,
        &mut slots,
    )?;
    // Loop bodies fill their skeleton-reserved slots after every chain fn;
    // fork-lifted bodies follow after every loop fn, wait-lifted bodies
    // after every fork fn (the reserved order).
    slots.loops.append_into(&mut skeleton.functions)?;
    slots.forks.append_into(&mut skeleton.functions)?;
    slots.waits.append_into(&mut skeleton.functions)?;
    // The shared dead-body function (T-DEAD) is a real, sidecar-visible entry
    // (S8): append exactly one when the module has any activity to close over.
    if !skeleton.plan.activities.is_empty() {
        skeleton.functions.push(build::dead_shell());
    }
    if skeleton.plan.child_witness.is_some() {
        skeleton.functions.push(build::child_witness_shell());
    }
    for predicate in ctx.take_predicates()? {
        skeleton.functions.push(predicate);
    }

    let mut module = MirModule {
        name: module_name,
        source,
        atoms: mem::take(&mut ctx.atoms),
        literals: mem::take(&mut ctx.literals),
        exports: skeleton.exports,
        functions: skeleton.functions,
        types: skeleton.types,
    };
    // S14: compute the backward-liveness y-spill contract over every body.
    super::liveness::annotate(&mut module);
    Ok(module)
}
