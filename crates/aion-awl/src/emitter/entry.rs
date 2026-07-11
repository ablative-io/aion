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

fn emit_inner(document: &Document, root: Option<&Path>) -> Result<String, EmitError> {
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
