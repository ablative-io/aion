//! The B2 lowering gate: the rev-3 flow shape (`subflow`, `distribute`/
//! `sequence`, `collect`, `max … visits`, value route payloads) parses and
//! checks today but does not lower yet — the emitter and the MIR lowering
//! refuse it honestly, with a span, before any planning runs. B4 (flow-
//! vocabulary lowering) removes this gate construct by construct.

use crate::Span;
use crate::ast::{Document, RoutePayload, RouteTarget, Statement, Step};

/// The first rev-3 flow-shape construct in the document, with the phrase
/// the refusal diagnostic names it by. `None` means the document uses only
/// the lowered surface.
pub(crate) fn first_unlowered(document: &Document) -> Option<(Span, &'static str)> {
    if let Some(subflow) = document.subflows.first() {
        return Some((subflow.name_span, "`subflow` declarations"));
    }
    for step in &document.steps {
        if let Some(found) = step_unlowered(step) {
            return Some(found);
        }
    }
    None
}

fn step_unlowered(step: &Step) -> Option<(Span, &'static str)> {
    if let Some(max_visits) = &step.max_visits {
        return Some((max_visits.span, "the `max … visits` step attribute"));
    }
    if let Some(found) = statements_unlowered(&step.body) {
        return Some(found);
    }
    if let Some(on_failure) = &step.on_failure
        && let Some(found) = statements_unlowered(&on_failure.body)
    {
        return Some(found);
    }
    for clause in &step.outcomes {
        if let Some(found) = route_unlowered(&clause.route) {
            return Some(found);
        }
    }
    None
}

fn statements_unlowered(statements: &[Statement]) -> Option<(Span, &'static str)> {
    for statement in statements {
        let found = match statement {
            Statement::Distribute(distribute) => {
                Some((distribute.span, "`distribute`/`sequence` regions"))
            }
            Statement::Collect(collect) => Some((collect.span, "`collect` steps")),
            Statement::Route(route) => route_unlowered(&route.target),
            Statement::Pipe(pipe) => match &pipe.end {
                crate::ast::PipeEnd::Route(target) => route_unlowered(target),
                crate::ast::PipeEnd::Bind(_) => None,
            },
            Statement::Fork(fork) => statements_unlowered(&fork.body),
            Statement::Loop(looped) => statements_unlowered(&looped.body),
            Statement::SubStep(sub) => step_unlowered(sub),
            Statement::Call(_) | Statement::Spawn(_) | Statement::Wait(_) | Statement::Sleep(_) => {
                None
            }
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

fn route_unlowered(target: &RouteTarget) -> Option<(Span, &'static str)> {
    match &target.payload {
        Some(RoutePayload::Value(value)) => Some((
            crate::spanned::Spanned::span(value),
            "value route payloads (`route out(<value>)`)",
        )),
        _ => None,
    }
}
