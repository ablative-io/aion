//! The MIR module: the reified catalog (AWL-BC-IR.md §2.3 / Appendix A).
//!
//! There is NO sidecar field — the `.gleam_types` bytes are
//! `project_sidecar(&MirModule)` (S2). Exports are exactly `run/1`,
//! `definition/0`, `execute/1` (no `module_info`, decision 12 / capstone
//! obs. 4).

use super::func::MirFn;
use super::ids::FnRef;
use super::shapes::{MirLiteral, TypeShape};

/// A complete lowered workflow module, private to `aion-awl` (decision 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirModule {
    /// Logical module name (snake of the workflow name).
    pub name: String,
    /// The `.awl` source file name (Line chunk file 0).
    pub source: String,
    pub atoms: Vec<String>,
    pub literals: Vec<MirLiteral>,
    /// Exactly `run/1`, `definition/0`, `execute/1`.
    pub exports: Vec<FnRef>,
    pub functions: Vec<MirFn>,
    pub types: Vec<TypeShape>,
}

impl MirModule {
    pub(crate) fn function(&self, reference: FnRef) -> Option<&MirFn> {
        self.functions.get(reference.0 as usize)
    }

    pub(crate) fn atom(&self, index: u32) -> Option<&str> {
        self.atoms.get(index as usize).map(String::as_str)
    }

    /// The physical BEAM arity of a function (used by `verify` for local-call
    /// arity checks and by the sidecar): the length of its parameter-type
    /// signature (declared params + appended captures for lifted closures).
    pub(crate) fn arity(function: &MirFn) -> u32 {
        u32::try_from(function.param_tys().len()).unwrap_or(u32::MAX)
    }
}
