//! Public checking entry points, orchestrating the declaration pass, the
//! graph pass, and the flow walk.

use std::path::Path;

use crate::ast::Document;

use super::context::Ctx;
use super::error::CheckError;
use super::{decls, graph, walk};

/// Typecheck a parsed document with no document directory: schema imports
/// (`type X = schema("file")`) cannot resolve and are reported as errors.
/// Prefer [`check_in`] when the document's directory is known.
#[must_use]
pub fn check(document: &Document) -> Vec<CheckError> {
    run(document, None)
}

/// Typecheck a parsed document, resolving schema imports relative to `root`
/// (the directory containing the `.awl` file).
#[must_use]
pub fn check_in(document: &Document, root: &Path) -> Vec<CheckError> {
    run(document, Some(root))
}

fn run(document: &Document, root: Option<&Path>) -> Vec<CheckError> {
    let mut ctx = Ctx::new(document, root);
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
    errors
}
