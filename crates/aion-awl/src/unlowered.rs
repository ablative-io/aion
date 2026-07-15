//! The lowering gate after B4: the rev-3 flow shape (`subflow`,
//! `distribute`/`sequence`, `collect`, `max … visits`, value route payloads)
//! lowers on both backends now — the one honest refusal left is `after`
//! dependencies on substeps, which the emitter would silently drop today.
//! That refusal stays until a separate landing lowers substep dependencies
//! faithfully.

use crate::Span;
use crate::ast::{Document, Statement, Step};

/// The first still-unlowered construct in the document, with the phrase the
/// refusal diagnostic names it by. `None` means the whole surface lowers.
pub(crate) fn first_unlowered(document: &Document) -> Option<(Span, &'static str)> {
    for step in &document.steps {
        if let Some(found) = step_unlowered(step) {
            return Some(found);
        }
    }
    for subflow in &document.subflows {
        for step in &subflow.steps {
            if let Some(found) = step_unlowered(step) {
                return Some(found);
            }
        }
    }
    None
}

fn step_unlowered(step: &Step) -> Option<(Span, &'static str)> {
    if let Some(found) = statements_unlowered(&step.body) {
        return Some(found);
    }
    if let Some(on_failure) = &step.on_failure {
        return statements_unlowered(&on_failure.body);
    }
    None
}

fn statements_unlowered(statements: &[Statement]) -> Option<(Span, &'static str)> {
    for statement in statements {
        let found = match statement {
            Statement::Fork(fork) => statements_unlowered(&fork.body),
            Statement::Loop(looped) => statements_unlowered(&looped.body),
            Statement::SubStep(sub) => match sub.after.first() {
                // The emitter drops nested `after` on the floor today —
                // refusing is honest; lowering it faithfully is future work.
                Some(dependency) => Some((dependency.span, "`after` dependencies on substeps")),
                None => step_unlowered(sub),
            },
            Statement::Call(_)
            | Statement::Spawn(_)
            | Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::Pipe(_)
            | Statement::Route(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}
