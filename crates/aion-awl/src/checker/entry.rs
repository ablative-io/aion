//! Public checking entry points, orchestrating the declaration pass, the
//! graph pass, and the flow walk.

use std::path::Path;

use crate::ast::Document;
use crate::semantic::SemanticAnalysis;

use super::context::Ctx;
use super::error::CheckError;
use super::{decls, graph, walk};

/// Typecheck a parsed document with no document directory: schema imports
/// (`type X = schema("file")`) cannot resolve and are reported as errors.
/// Prefer [`check_in`] when the document's directory is known.
#[must_use]
pub fn check(document: &Document) -> Vec<CheckError> {
    run(document, None).0
}

/// Typecheck a parsed document, resolving schema imports relative to `root`
/// (the directory containing the `.awl` file).
#[must_use]
pub fn check_in(document: &Document, root: &Path) -> Vec<CheckError> {
    run(document, Some(root)).0
}

pub(crate) fn analyze(document: &Document, root: Option<&Path>) -> SemanticAnalysis {
    let (diagnostics, builder) = run(document, root);
    SemanticAnalysis::from_parts(diagnostics, builder)
}

fn run(document: &Document, root: Option<&Path>) -> (Vec<CheckError>, crate::semantic::Builder) {
    let mut ctx = Ctx::new(document, root);
    if let Some(timeout) = &document.timeout
        && (timeout.negative || timeout.duration.magnitude == 0)
    {
        ctx.error(
            timeout.span,
            "workflow `timeout` must be a positive duration",
        );
    }
    decls::run(&mut ctx);
    let step_graph = graph::build(&mut ctx);
    if !step_graph.after_cycle {
        walk::run(&mut ctx, &step_graph);
    }
    let mut errors = ctx.errors;
    errors.sort_by(|a, b| {
        a.span
            .start
            .cmp(&b.span.start)
            .then_with(|| a.message.cmp(&b.message))
    });
    errors.dedup();
    (errors, ctx.semantic)
}
