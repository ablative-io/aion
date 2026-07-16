//! The emitter's public entry points and assembly order.
//!
//! Lowering runs first (control flow, wrappers, codecs) so the feature
//! flags are final; the module header, error type, type declarations,
//! `definition()`, and `run()` assemble afterwards, then the captured
//! sections append in reading order.

use std::mem;
use std::path::Path;

use crate::ast::Document;

use super::artifact::EmittedArtifact;
use super::bindings;
use super::codecs;
use super::context::Emitter;
use super::error::EmitError;
use super::flowshape::{self, Shaped};
use super::frame;
use super::graph::{self, Plans};
use super::steps;
use super::types::build_env;
use super::wrappers;

/// Emit a complete Gleam workflow module for a parsed AWL document, with
/// schema imports unresolvable (no document directory). Prefer [`emit_in`]
/// when the `.awl` file's directory is known.
///
/// # Errors
///
/// Returns [`EmitError`] for constructs the Gleam stopgap cannot lower
/// faithfully, and for documents that would not check cleanly.
pub fn emit(document: &Document) -> Result<String, EmitError> {
    Ok(emit_artifact(document)?.source)
}

/// Emit a complete Gleam workflow module, resolving schema imports relative
/// to `root` (the document's directory).
///
/// # Errors
///
/// Returns [`EmitError`] for constructs the Gleam stopgap cannot lower
/// faithfully, and for documents that would not check cleanly.
pub fn emit_in(document: &Document, root: &Path) -> Result<String, EmitError> {
    Ok(emit_artifact_in(document, root)?.source)
}

/// Emit source together with every synthesized same-package workflow entry.
///
/// # Errors
///
/// Returns [`EmitError`] under the same conditions as [`emit`].
pub fn emit_artifact(document: &Document) -> Result<EmittedArtifact, EmitError> {
    emit_inner(document, None)
}

/// Emit a structured artifact while resolving schema imports relative to `root`.
///
/// # Errors
///
/// Returns [`EmitError`] under the same conditions as [`emit_in`].
pub fn emit_artifact_in(document: &Document, root: &Path) -> Result<EmittedArtifact, EmitError> {
    emit_inner(document, Some(root))
}

/// Fold, then shape, a checked document: the one preparation both backends
/// run before planning (D-BC1 — decisions shared, zero drift).
pub(crate) fn shape_document(
    document: &Document,
    root: Option<&Path>,
) -> Result<Shaped, EmitError> {
    let folded = crate::fold::fold_document(document, root)
        .map_err(|error| EmitError::new(error.span, error.message))?;
    flowshape::shape(&folded).map_err(|error| EmitError::new(error.span, error.message))
}

/// Run the shared planning passes (`build_env`, `bindings::compute`,
/// `graph::plan_all`) over a shaped document and return the populated
/// [`Emitter`] context together with every flow's [`Plans`]. This is the
/// single lowering front the AWL-BC MIR backend consumes (D-BC1 /
/// AWL-BC-IR.md §4 lowering rule zero): both the Gleam emitter and the
/// bytecode `lower` derive regions, Kahn layers, liveness-threaded params,
/// and refusals from these exact passes, so those decisions cannot drift
/// between backends.
pub(crate) fn prepare<'a>(
    shaped: &'a Shaped,
    root: Option<&Path>,
) -> Result<(Emitter<'a>, Plans<'a>), EmitError> {
    let env = build_env(&shaped.document, root)?;
    let mut emitter = Emitter::new(
        &shaped.document,
        env,
        &shaped.host_regions,
        &shaped.subflows,
        &shaped.generated_names,
    )?;
    bindings::compute(&mut emitter)?;
    let plans = graph::plan_all(&emitter)?;
    Ok((emitter, plans))
}

fn emit_inner(document: &Document, root: Option<&Path>) -> Result<EmittedArtifact, EmitError> {
    // Emission is defined only for documents that check cleanly: fold-time
    // const substitution is name-based and relies on the checker's
    // invariants (no shadowed consts, no input/signal collisions), so an
    // unchecked document could emit code with different semantics from the
    // checker-resolved program.
    let diagnostics = match root {
        Some(root) => crate::checker::check_in(document, root),
        None => crate::checker::check(document),
    };
    if let Some(first) = diagnostics.first() {
        return Err(EmitError::new(
            first.span,
            format!("document does not check cleanly: {}", first.message),
        ));
    }
    // The one rev-3 refusal B4 keeps: substep `after` dependencies would
    // drop on the floor — refuse honestly, with a span, before planning.
    if let Some((span, what)) = crate::unlowered::first_unlowered(document) {
        return Err(EmitError::new(
            span,
            format!("{what} are not yet lowered — a later landing carries them"),
        ));
    }
    // Fold the B1 ergonomics vocabulary down to plain literals, then shape
    // the rev-3 flow constructs (regions, subflows, visit counters) into
    // the planned form both backends lower.
    let shaped = shape_document(document, root)?;
    let (mut emitter, plans) = prepare(&shaped, root)?;

    let flow = emitter.capture(|this| steps::emit_flow(this, &plans))?;
    let wrapper_section = emitter.capture(|this| {
        wrappers::activity_wrappers(this);
        wrappers::signal_refs(this);
        Ok(())
    })?;
    let codec_section = emitter.capture(codecs::emit_codecs)?;

    frame::header(&mut emitter);
    frame::type_decls(&mut emitter);
    frame::definition(&mut emitter);
    frame::run(&mut emitter);
    emitter.out.push_str(&flow);
    let loop_fns = mem::take(&mut emitter.loop_fns);
    for loop_fn in loop_fns {
        emitter.out.push_str(&loop_fn);
        emitter.blank();
    }
    emitter.out.push_str(&wrapper_section);
    emitter.out.push_str(&codec_section);
    Ok(EmittedArtifact {
        entry_module: document.name.clone(),
        source: emitter.out,
        synthesized_workflows: emitter.synthesized_workflows,
    })
}
