//! Runtime module registration helpers.

use beamr::atom::Atom;
use beamr::loader::decode::compact::Operand;
use beamr::loader::{lambda_unique_id, prepare_module, Instruction, Literal};
use beamr::module::ResolvedImportTarget;

use crate::{EngineError, RuntimeHandle};

impl RuntimeHandle {
    /// Register transformed BEAM bytes under their already-deployed module name.
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
        let deployed_atom = self.atom_table.intern(deployed_name);
        if self.module_registry.lookup_old(deployed_atom).is_some() {
            return Err(runtime_error(format!(
                "cannot register deployed module `{deployed_name}` while old code is still retained"
            )));
        }

        let (mut module, _unresolved) = prepare_module(
            beam_bytes,
            &self.atom_table,
            &self.module_registry,
            self.native_registry.as_ref(),
        )
        .map_err(runtime_error_from_display)?;
        rename_module_references(&mut module, deployed_atom, &self.atom_table)?;
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
    atom_table: &beamr::atom::AtomTable,
) -> Result<(), EngineError> {
    let original_atom = module.name;
    if original_atom == deployed_atom {
        return Ok(());
    }

    module.name = deployed_atom;
    for import in &mut module.resolved_imports {
        if import.module == original_atom {
            import.module = deployed_atom;
        }
        rewrite_resolved_import_target(&mut import.target, original_atom, deployed_atom);
    }
    for instruction in &mut module.code {
        rewrite_instruction_module_operand(instruction, original_atom, deployed_atom);
    }
    for literal in &mut module.literals {
        rewrite_literal_atom(literal, original_atom, deployed_atom);
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

fn rewrite_resolved_import_target(
    target: &mut ResolvedImportTarget,
    original_atom: Atom,
    deployed_atom: Atom,
) {
    match target {
        ResolvedImportTarget::Code { module, .. } => {
            if *module == original_atom {
                *module = deployed_atom;
            }
        }
        ResolvedImportTarget::Deferred { module, .. }
        | ResolvedImportTarget::Unresolved { module, .. } => {
            if *module == original_atom {
                *module = deployed_atom;
            }
        }
        ResolvedImportTarget::Native(_) => {}
    }
}

fn rewrite_instruction_module_operand(
    instruction: &mut Instruction,
    original_atom: Atom,
    deployed_atom: Atom,
) {
    if let Instruction::FuncInfo { module, .. } = instruction {
        rewrite_operand_atom(module, original_atom, deployed_atom);
    }
}

fn rewrite_operand_atom(operand: &mut Operand, original_atom: Atom, deployed_atom: Atom) {
    match operand {
        Operand::Atom(Some(atom)) if *atom == original_atom => *atom = deployed_atom,
        Operand::Literal(literal) => rewrite_literal_atom(literal, original_atom, deployed_atom),
        Operand::List(items) => {
            for item in items {
                rewrite_operand_atom(item, original_atom, deployed_atom);
            }
        }
        Operand::TypedRegister { register, .. } => {
            rewrite_operand_atom(register, original_atom, deployed_atom);
        }
        Operand::Integer(_)
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

fn rewrite_literal_atom(literal: &mut Literal, original_atom: Atom, deployed_atom: Atom) {
    match literal {
        Literal::Atom(atom) if *atom == original_atom => *atom = deployed_atom,
        Literal::Tuple(items) => {
            for item in items {
                rewrite_literal_atom(item, original_atom, deployed_atom);
            }
        }
        Literal::List(items, tail) => {
            for item in items {
                rewrite_literal_atom(item, original_atom, deployed_atom);
            }
            rewrite_literal_atom(tail, original_atom, deployed_atom);
        }
        Literal::Map(entries) => {
            for (key, value) in entries {
                rewrite_literal_atom(key, original_atom, deployed_atom);
                rewrite_literal_atom(value, original_atom, deployed_atom);
            }
        }
        Literal::Integer(_)
        | Literal::Float(_)
        | Literal::BigInteger(_)
        | Literal::Atom(_)
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
