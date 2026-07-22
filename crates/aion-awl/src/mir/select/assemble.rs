//! Assembly + the BC-3 self-gate oracle (AWL-BC-IR.md §11.5). Lowers every MIR
//! function (flow or shell) to a [`Body`], emits the module-wide instruction
//! stream, builds a `ParsedModule` (chunk set / order owned by the encoder),
//! encodes to `.beam` bytes, and re-loads + `validate_module`s the result
//! through the loader's five layers — a rejection is a hard error, never a
//! silent artifact.

use beamr::atom::{Atom, AtomTable};
use beamr::loader::encode::encode_module;
use beamr::loader::load::{ParsedModule, resolve_imports};
use beamr::loader::validate::validate_module;
use beamr::loader::{ExportEntry, load_beam_chunks};
use beamr::module::ModuleRegistry;
use beamr::native::{AllCapabilitiesPolicy, BifRegistry, NativeEntry};

use crate::mir::{FnRef, MirFn, MirModule};

use super::builder::Builder;
use super::emit::emit_body;
use super::error::SelectError;
use super::flow::lower_flow;
use super::shells::{find_execute, lower_shell};

/// Lower, emit, encode, and self-gate a MIR module into `.beam` bytes.
pub(super) fn assemble(module: &MirModule) -> Result<Vec<u8>, SelectError> {
    let mut builder = Builder::new(module);
    let execute = find_execute(module)?;

    let mut bodies = Vec::with_capacity(module.functions.len() + 1);
    for (index, function) in module.functions.iter().enumerate() {
        let reference = FnRef(u32::try_from(index).unwrap_or(u32::MAX));
        let body = match function {
            MirFn::Templated {
                name,
                template,
                sig,
                ..
            } => {
                let arity =
                    u8::try_from(sig.params.len()).map_err(|_| SelectError::OutOfRange {
                        what: format!("`{name}` arity exceeds 255"),
                    })?;
                lower_shell(&mut builder, name, arity, template, reference, execute)?
            }
            MirFn::Flow(flow) => lower_flow(&mut builder, flow, reference)?,
        };
        bodies.push(body);
    }

    let mut instructions = Vec::new();
    for body in &bodies {
        instructions.extend(emit_body(&mut builder, body)?);
    }

    let exports = build_exports(&mut builder, module)?;
    let module_atom = builder.atom(&module.name.clone());
    let parts = builder.into_parts();

    let parsed = ParsedModule {
        name: module_atom,
        atoms: parts.atoms,
        instructions,
        imports: parts.imports,
        exports,
        lambdas: parts.lambdas,
        literals: parts.literals,
        string_table: Vec::new(),
        line_info: Vec::new(),
    };

    let bytes = encode_module(&parsed, &parts.atom_table)?;
    self_gate(&bytes, &parts.atom_table)?;
    Ok(bytes)
}

/// Lowers every MIR function (flow or shell) to its selected [`Body`] and returns
/// them alongside the emit-side atom table — the SAME lowering `assemble` runs,
/// stopped before emission. The BC-5 marshaling oracle carries these selected
/// call/argument expectations INDEPENDENTLY into the decoded check (BC-5 review
/// blocker 6); the atom table resolves an expected `Src::Atom` argument to its
/// name for comparison against the decode-side table.
///
/// # Errors
///
/// Propagates a lowering refusal or an arity overflow.
#[cfg(test)]
pub(super) fn lower_bodies(
    module: &MirModule,
) -> Result<(Vec<super::ir::Body>, AtomTable), SelectError> {
    let mut builder = Builder::new(module);
    let execute = find_execute(module)?;
    let mut bodies = Vec::with_capacity(module.functions.len());
    for (index, function) in module.functions.iter().enumerate() {
        let reference = FnRef(u32::try_from(index).unwrap_or(u32::MAX));
        let body = match function {
            MirFn::Templated {
                name,
                template,
                sig,
                ..
            } => {
                let arity =
                    u8::try_from(sig.params.len()).map_err(|_| SelectError::OutOfRange {
                        what: format!("`{name}` arity exceeds 255"),
                    })?;
                lower_shell(&mut builder, name, arity, template, reference, execute)?
            }
            MirFn::Flow(flow) => lower_flow(&mut builder, flow, reference)?,
        };
        bodies.push(body);
    }
    Ok((bodies, builder.into_parts().atom_table))
}

/// The `ExpT` chunk: exactly the module's declared exports (`run/1`,
/// `definition/0`, `execute/1` — decision 12), each at its body label.
fn build_exports(
    builder: &mut Builder<'_>,
    module: &MirModule,
) -> Result<Vec<ExportEntry>, SelectError> {
    let mut exports = Vec::with_capacity(module.exports.len());
    for reference in &module.exports {
        let function = module
            .function(*reference)
            .ok_or_else(|| SelectError::invariant("export ref out of range"))?;
        let name = function.name().to_owned();
        let arity =
            u8::try_from(MirModule::arity(function)).map_err(|_| SelectError::OutOfRange {
                what: "export arity".to_owned(),
            })?;
        exports.push(ExportEntry {
            function: builder.atom(&name),
            arity,
            label: Builder::fn_labels(*reference).body,
        });
    }
    Ok(exports)
}

/// An empty BIF registry — imports resolve to deferred targets, which
/// `validate_module` accepts (the same standalone-validation setup the BC-1
/// capstone used).
struct NoBifs;

impl BifRegistry for NoBifs {
    fn lookup(&self, _module: Atom, _function: Atom, _arity: u8) -> Option<NativeEntry> {
        None
    }
}

/// The BC-3 oracle: every emitted module must re-load and validate through all
/// five loader layers (`load_beam_chunks` → `resolve_imports` →
/// `validate_module`).
fn self_gate(bytes: &[u8], atom_table: &AtomTable) -> Result<(), SelectError> {
    let parsed = load_beam_chunks(bytes, atom_table)
        .map_err(|error| SelectError::Load(error.to_string()))?;
    let registry = ModuleRegistry::new();
    let (resolved, _report) = resolve_imports(&parsed, &registry, &NoBifs, &AllCapabilitiesPolicy);
    validate_module(&parsed, &resolved)
        .map_err(|error| SelectError::Validate(format!("{error:?}")))?;
    Ok(())
}
