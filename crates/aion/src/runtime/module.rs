//! Runtime module registration helpers.

use std::collections::HashMap;

use beamr::atom::Atom;
use beamr::loader::decode::compact::Operand;
use beamr::loader::{Instruction, Literal, lambda_unique_id, prepare_module};
use beamr::module::ResolvedImportTarget;

use crate::{EngineError, RuntimeHandle};

/// Atom-level rename map from original module names to deployed names.
///
/// Built by [`RuntimeHandle::package_rename_map`] from a package's module
/// list and consumed by [`RuntimeHandle::register_module_with_renames`].
pub type ModuleRenameMap = HashMap<Atom, Atom>;

impl RuntimeHandle {
    /// Register transformed BEAM bytes under their already-deployed module name.
    ///
    /// Only rewrites self-references. Use [`Self::register_module_with_renames`]
    /// for package loads where cross-module imports need the full rename map.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when beamr cannot prepare the module bytes
    /// or when the deployed name still has retained old code in the registry.
    pub fn register_module(
        &self,
        deployed_name: &str,
        beam_bytes: &[u8],
    ) -> Result<(), EngineError> {
        let mut self_only = HashMap::new();
        let deployed_atom = self.atom_table.intern(deployed_name);
        let (module, _) = prepare_module(
            beam_bytes,
            &self.atom_table,
            &self.module_registry,
            self.native_registry.as_ref(),
        )
        .map_err(runtime_error_from_display)?;
        self_only.insert(module.name, deployed_atom);
        self.register_prepared_module(module, deployed_name, &self_only)
    }

    /// Register BEAM bytes with a full package rename map for cross-module
    /// import rewriting.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when beamr cannot prepare the module bytes
    /// or when the deployed name still has retained old code in the registry.
    pub fn register_module_with_renames(
        &self,
        deployed_name: &str,
        beam_bytes: &[u8],
        rename_map: &ModuleRenameMap,
    ) -> Result<(), EngineError> {
        let (module, unresolved) = prepare_module(
            beam_bytes,
            &self.atom_table,
            &self.module_registry,
            self.native_registry.as_ref(),
        )
        .map_err(runtime_error_from_display)?;
        if !unresolved.is_empty() {
            tracing::warn!(
                module = deployed_name,
                count = unresolved.imports().len(),
                "unresolved BEAM imports after BIF + module registration"
            );
        }
        self.register_prepared_module(module, deployed_name, rename_map)
    }

    /// Build the atom-level rename map for every module in a package.
    #[must_use]
    pub fn package_rename_map(
        &self,
        original_names: &[&str],
        deployed_names: &[&str],
    ) -> ModuleRenameMap {
        original_names
            .iter()
            .zip(deployed_names.iter())
            .map(|(original, deployed)| {
                (
                    self.atom_table.intern(original),
                    self.atom_table.intern(deployed),
                )
            })
            .collect()
    }

    fn register_prepared_module(
        &self,
        mut module: beamr::module::Module,
        deployed_name: &str,
        rename_map: &ModuleRenameMap,
    ) -> Result<(), EngineError> {
        let deployed_atom = self.atom_table.intern(deployed_name);
        if self.module_registry.lookup_old(deployed_atom).is_some() {
            return Err(runtime_error(format!(
                "cannot register deployed module `{deployed_name}` while old code is still retained"
            )));
        }
        rename_module_references(&mut module, deployed_atom, rename_map, &self.atom_table)?;
        self.module_registry.insert(module);
        Ok(())
    }

    /// Return true when a module has been registered in the embedded module registry.
    #[must_use]
    pub fn has_registered_module(&self, deployed_name: &str) -> bool {
        let module = self.atom_table.intern(deployed_name);
        self.module_registry.lookup(module).is_some()
    }

    /// Remove a module registered during a failed staged package load.
    pub(crate) fn unregister_module(&self, deployed_name: &str) -> Result<(), EngineError> {
        let module = self.atom_table.intern(deployed_name);
        if self.module_registry.delete_module(module) {
            Ok(())
        } else {
            Err(runtime_error(format!(
                "module `{deployed_name}` was not registered"
            )))
        }
    }
}

fn rename_module_references(
    module: &mut beamr::module::Module,
    deployed_atom: Atom,
    rename_map: &ModuleRenameMap,
    atom_table: &beamr::atom::AtomTable,
) -> Result<(), EngineError> {
    if module.name == deployed_atom && rename_map.len() <= 1 {
        return Ok(());
    }

    module.name = deployed_atom;
    for import in &mut module.resolved_imports {
        if let Some(&new_atom) = rename_map.get(&import.module) {
            import.module = new_atom;
        }
        rewrite_resolved_import_target(&mut import.target, rename_map);
    }
    for instruction in &mut module.code {
        rewrite_instruction_module_operand(instruction, rename_map);
    }
    for literal in &mut module.literals {
        rewrite_literal_atom(literal, rename_map);
    }
    for lambda in &mut module.lambdas {
        lambda.unique_id = lambda_unique_id(
            atom_table,
            deployed_atom,
            lambda.function,
            lambda.arity,
            lambda.num_free,
        )
        .map_err(runtime_error_from_display)?;
    }

    Ok(())
}

fn rewrite_resolved_import_target(target: &mut ResolvedImportTarget, rename_map: &ModuleRenameMap) {
    match target {
        ResolvedImportTarget::Code { module, .. }
        | ResolvedImportTarget::Deferred { module, .. }
        | ResolvedImportTarget::Unresolved { module, .. } => {
            if let Some(&new_atom) = rename_map.get(module) {
                *module = new_atom;
            }
        }
        ResolvedImportTarget::Native(_) => {}
    }
}

fn rewrite_instruction_module_operand(instruction: &mut Instruction, rename_map: &ModuleRenameMap) {
    if let Instruction::FuncInfo { module, .. } = instruction {
        rewrite_operand_atom(module, rename_map);
    }
}

fn rewrite_operand_atom(operand: &mut Operand, rename_map: &ModuleRenameMap) {
    match operand {
        Operand::Atom(Some(atom)) => {
            if let Some(&new_atom) = rename_map.get(atom) {
                *atom = new_atom;
            }
        }
        Operand::List(items) => {
            for item in items {
                rewrite_operand_atom(item, rename_map);
            }
        }
        Operand::TypedRegister { register, .. } => {
            rewrite_operand_atom(register, rename_map);
        }
        Operand::Literal(_)
        | Operand::Integer(_)
        | Operand::Unsigned(_)
        | Operand::Atom(_)
        | Operand::X(_)
        | Operand::Y(_)
        | Operand::Label(_)
        | Operand::Character(_)
        | Operand::FloatRegister(_)
        | Operand::Allocation(_) => {}
    }
}

fn rewrite_literal_atom(literal: &mut Literal, rename_map: &ModuleRenameMap) {
    match literal {
        Literal::Atom(atom) => {
            if let Some(&new_atom) = rename_map.get(atom) {
                *atom = new_atom;
            }
        }
        Literal::Tuple(items) => {
            for item in items {
                rewrite_literal_atom(item, rename_map);
            }
        }
        Literal::List(items, tail) => {
            for item in items {
                rewrite_literal_atom(item, rename_map);
            }
            rewrite_literal_atom(tail, rename_map);
        }
        Literal::Map(entries) => {
            for (key, value) in entries {
                rewrite_literal_atom(key, rename_map);
                rewrite_literal_atom(value, rename_map);
            }
        }
        Literal::Integer(_)
        | Literal::Float(_)
        | Literal::BigInteger(_)
        | Literal::Binary(_)
        | Literal::Nil
        | Literal::String(_) => {}
    }
}

fn runtime_error(reason: String) -> EngineError {
    EngineError::Runtime { reason }
}

fn runtime_error_from_display(reason: impl std::fmt::Display) -> EngineError {
    runtime_error(reason.to_string())
}
