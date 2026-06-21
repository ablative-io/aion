//! Extract a workflow's primitive structure from its entry-module Gleam source.
//!
//! A Rust crate cannot type-check Gleam, so — following the codegen precedent
//! (a generator operates over a canonical derived representation, never by
//! type-checking Gleam) — extraction scans the entry-module source for the
//! fixed `aion/workflow` primitive vocabulary. This is sound only because that
//! vocabulary is small and known (ADR-014, P2). The supported subset is stated
//! and enforced below; anything outside it is reported, never silently
//! mis-parsed (no silent failures).
//!
//! ## Supported import/call subset
//!
//! - The entry module imports `aion/workflow`, optionally aliased
//!   (`import aion/workflow as <alias>`). Primitive calls are recognised as
//!   `<alias>.run`, `<alias>.all`, and so on. Absence of the import is a loud
//!   [`StructureError::NoWorkflowImport`], never an empty graph.
//! - A `run` node's activity name is read from the
//!   `<wrappers_alias>.<name>_activity(` call passed to `run`, where the
//!   wrappers module is imported as `import <pkg>_activity_wrappers as <alias>`
//!   (the form `aion generate` emits). The extracted name is validated against
//!   the manifest's declared activities.
//! - `start_timer`, `spawn`, and `spawn_and_wait` carry their first string
//!   literal argument as a name; it is read directly from source.
//!
//! ## Control flow
//!
//! Extraction is control-flow faithful (the [`super::control_flow`] walker). It
//! starts at the manifest entry function, recurses the body in source order, and
//! models a `case` over a workflow-primitive result as a [`NodePrimitive::Branch`]
//! with labelled `Ok` / `Error` arm edges into the real success and compensation
//! subgraphs — following local-helper calls so a saga's compensations land on
//! the error arm, not in a false linear sequence. Control flow it cannot resolve
//! is surfaced as an explicit [`NodePrimitive::Opaque`] node, never flattened.

use crate::Package;

use super::control_flow::ControlFlowExtractor;
use super::error::StructureError;
use super::model::WorkflowGraph;
use super::scan::{Token, tokenise};

/// Extracts the workflow graph model from a loaded package.
///
/// Reads the manifest entry module's verbatim Gleam source from
/// [`Package::source`], walks it from the entry function over the
/// `aion/workflow` primitive vocabulary, and builds a control-flow-faithful
/// node/edge graph whose nodes carry the correlation key a consumer overlays a
/// run's recorded events onto (C21, C22, C23).
///
/// # Errors
///
/// Returns [`StructureError::MissingEntrySource`] when the entry module has no
/// source, [`StructureError::EntrySourceNotUtf8`] when its bytes are not UTF-8,
/// [`StructureError::NoWorkflowImport`] when it does not import `aion/workflow`,
/// [`StructureError::EntryFunctionNotFound`] when the manifest entry function is
/// not defined in the source, and [`StructureError::UnknownActivity`] when a
/// `run` node names an activity the manifest does not declare.
pub fn extract_structure(package: &Package) -> Result<WorkflowGraph, StructureError> {
    let entry_module = package.manifest().entry_module.clone();
    let entry_function = package.manifest().entry_function.clone();
    let bytes =
        package
            .source()
            .get(&entry_module)
            .ok_or_else(|| StructureError::MissingEntrySource {
                module: entry_module.clone(),
            })?;
    let source = std::str::from_utf8(bytes).map_err(|_| StructureError::EntrySourceNotUtf8 {
        module: entry_module.clone(),
    })?;

    let tokens = tokenise(source);
    let workflow_alias =
        workflow_alias(&tokens).ok_or_else(|| StructureError::NoWorkflowImport {
            module: entry_module.clone(),
        })?;

    let declared: std::collections::BTreeSet<&str> = package
        .manifest()
        .activities
        .iter()
        .map(|activity| activity.activity_type.as_str())
        .collect();

    let extractor =
        ControlFlowExtractor::new(entry_module.clone(), &tokens, workflow_alias, &declared);
    let extracted = extractor.extract(&entry_function)?;

    Ok(WorkflowGraph {
        entry_module,
        nodes: extracted.nodes,
        edges: extracted.edges,
    })
}

/// Finds the alias the entry module imports `aion/workflow` under.
///
/// `import aion/workflow` yields `workflow` (the last path segment); an explicit
/// `import aion/workflow as <alias>` yields `<alias>`. Returns `None` when the
/// module does not import `aion/workflow` at all.
fn workflow_alias(tokens: &[Token]) -> Option<String> {
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], Token::Ident(word) if word == "import")
            && import_path_is_workflow(tokens, index + 1)
        {
            if let Some(alias) = alias_after_import(tokens, index + 1) {
                return Some(alias);
            }
            return Some("workflow".to_owned());
        }
        index += 1;
    }
    None
}

/// Whether the import path tokens beginning at `start` spell `aion/workflow`.
///
/// The scanner renders `aion/workflow` as `Ident("aion")`, `Other('/')`,
/// `Ident("workflow")`; an aliased or selective import keeps the same path
/// prefix, so matching the two path identifiers around the slash is sufficient
/// and avoids matching `aion/workflow/timer` style submodule imports the entry
/// module does not orchestrate through.
fn import_path_is_workflow(tokens: &[Token], start: usize) -> bool {
    matches!(
        (tokens.get(start), tokens.get(start + 1), tokens.get(start + 2)),
        (
            Some(Token::Ident(first)),
            Some(Token::Other('/')),
            Some(Token::Ident(second)),
        ) if first == "aion"
            && second == "workflow"
            && !matches!(tokens.get(start + 3), Some(Token::Other('/')))
    )
}

/// Reads the alias of `import aion/workflow as <alias>`.
///
/// The path occupies three tokens (`aion`, `/`, `workflow`) from `path_start`;
/// an alias, if present, is the token after a literal `as` immediately
/// following the path. Anything else (a bare import, a selective
/// `import aion/workflow.{...}`, or the next statement) yields no alias here and
/// the caller falls back to the default `workflow`.
fn alias_after_import(tokens: &[Token], path_start: usize) -> Option<String> {
    let after_path = path_start + 3;
    if matches!(tokens.get(after_path), Some(Token::Ident(word)) if word == "as") {
        if let Some(Token::Ident(alias)) = tokens.get(after_path + 1) {
            return Some(alias.clone());
        }
    }
    None
}
