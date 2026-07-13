//! BC-3 — selection, register allocation, and assembly (AWL-BC-IR.md §10–§11).
//!
//! `select` consumes a verified [`crate::mir::MirModule`], expands the template
//! shells (§2.4) and lowers every flow function into a resolved, register-free
//! body (`ir`/`flow`/`shells`), runs the two-tier register allocator + burst
//! emitter (`emit`), assembles the instruction stream + pools into a beamr
//! `ParsedModule`, encodes it to `.beam` bytes through
//! `beamr::loader::encode::encode_module` (0.14.0, feature `encode`), and
//! self-gates the result through all five loader layers (`assemble`). A shape
//! BC-3 does not yet emit is an honest span-anchored `Unsupported` refusal
//! (D-BC3), never a skip or a silent wrong artifact.

mod assemble;
mod builder;
mod emit;
mod error;
mod flow;
mod ir;
mod shells;

#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod tests;

pub use error::SelectError;

use crate::mir::MirModule;

/// Select instructions for a verified MIR module and assemble `.beam` bytes.
///
/// The returned bytes are proven — every module `select` returns has passed
/// `load_beam_chunks` → `resolve_imports` → `validate_module` (the BC-3
/// oracle). Determinism: the whole pipeline is a pure function of the
/// `MirModule`, so the same `.awl` yields the same bytes (#218 holds through
/// BC-3).
pub fn select(module: &MirModule) -> Result<Vec<u8>, SelectError> {
    assemble::assemble(module)
}
