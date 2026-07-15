//! The emitter's public entry points and assembly order.
//!
//! Lowering runs first (control flow, wrappers, codecs) so the feature
//! flags are final; the module header, error type, type declarations,
//! `definition()`, and `run()` assemble afterwards, then the captured
//! sections append in reading order.

use std::mem;
use std::path::Path;

use crate::ast::Document;

use super::bindings;
use super::codecs;
use super::context::Emitter;
use super::error::EmitError;
use super::frame;
use super::graph;
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
    emit_inner(document, None)
}

/// Emit a complete Gleam workflow module, resolving schema imports relative
/// to `root` (the document's directory).
///
/// # Errors
///
/// Returns [`EmitError`] for constructs the Gleam stopgap cannot lower
/// faithfully, and for documents that would not check cleanly.
pub fn emit_in(document: &Document, root: &Path) -> Result<String, EmitError> {
    emit_inner(document, Some(root))
}

/// Run the shared planning passes (`build_env`, `bindings::compute`,
/// `graph::plan`) and return the populated [`Emitter`] context together with
/// its control-flow [`Plan`]. This is the single lowering front the AWL-BC
/// MIR backend consumes (D-BC1 / AWL-BC-IR.md §4 lowering rule zero): both the
/// Gleam emitter and the bytecode `lower` derive regions, Kahn layers,
/// liveness-threaded params, and refusals from these exact passes, so those
/// decisions cannot drift between backends.
pub(crate) fn prepare<'a>(
    document: &'a Document,
    root: Option<&Path>,
) -> Result<(Emitter<'a>, graph::Plan), EmitError> {
    let env = build_env(document, root)?;
    let mut emitter = Emitter::new(document, env)?;
    bindings::compute(&mut emitter)?;
    let plan = graph::plan(&emitter)?;
    Ok((emitter, plan))
}

fn emit_inner(document: &Document, root: Option<&Path>) -> Result<String, EmitError> {
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
    // The rev-3 flow shape (B2) checks but is not yet lowered: refuse it
    // honestly here — with a span — before any planning pass runs.
    if let Some((span, what)) = crate::unlowered::first_unlowered(document) {
        return Err(EmitError::new(
            span,
            format!("{what} are not yet lowered — flow-vocabulary lowering lands in B4"),
        ));
    }
    // Fold the B1 ergonomics vocabulary (raw strings, `json { … }`,
    // `schema of`, const references) down to plain literals first, so the
    // lowering below only ever sees the vocabulary it already speaks.
    let document = &crate::fold::fold_document(document, root)
        .map_err(|error| EmitError::new(error.span, error.message))?;
    let env = build_env(document, root)?;
    let mut emitter = Emitter::new(document, env)?;
    bindings::compute(&mut emitter)?;
    let plan = graph::plan(&emitter)?;

    let flow = emitter.capture(|this| steps::emit_flow(this, &plan))?;
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
    Ok(emitter.out)
}
