//! Test-only helpers for runtime-backed activity modules.

use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ResolvedImport, ResolvedImportTarget};
use beamr::term::Term;

use super::{EngineError, Mfa, NifRegistration, RuntimeHandle, RuntimeInput};

impl RuntimeInput {
    pub(crate) fn from_payloads_for_test(
        payloads: &[aion_core::Payload],
    ) -> Result<Self, EngineError> {
        let mut combined = Self::default();
        for payload in payloads {
            let input = Self::from_payload(payload)?;
            combined.terms.extend(input.terms);
            combined.heaps.extend(input.heaps);
        }
        Ok(combined)
    }
}

impl RuntimeHandle {
    pub(crate) fn install_test_activity_nif(
        &self,
        module: &str,
        function: &str,
        dirty: bool,
        succeeds: bool,
    ) -> Result<(), EngineError> {
        let mut registration = NifRegistration::new();
        let native = if succeeds {
            test_activity_answer
        } else {
            test_activity_fail
        };
        let entry = if dirty {
            super::super::nif::NifEntry::dirty(Mfa::new(module, function, 1), native)
        } else {
            super::super::nif::NifEntry::new(Mfa::new(module, function, 1), native)
        };
        registration.add_host_nifs([entry]);
        self.install_nifs(registration)
    }

    pub(crate) fn register_native_call_module_for_test(
        &self,
        module_name: &str,
        function_name: &str,
        native_module: &str,
        native_function: &str,
    ) {
        let module = self.atom_table.intern(module_name);
        let function = self.atom_table.intern(function_name);
        let target_module = self.atom_table.intern(native_module);
        let target_function = self.atom_table.intern(native_function);
        let native_entry = self.lookup_native_for_test(native_module, native_function, 1);
        let label = 1;
        let code = vec![
            Instruction::Label { label },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ];
        let mut module_data = Module {
            name: module,
            generation: 0,
            origin: beamr::module::ModuleOrigin::Preloaded,
            exports: std::collections::HashMap::from([((function, 1), label)]),
            label_index: std::collections::HashMap::from([(label, 0)]),
            code,
            function_table: Vec::new(),
            line_table: Vec::new(),
            literals: Vec::new(),
            constant_pool: beamr::constant_pool::ConstantPool::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        };
        if let Some(native_entry) = native_entry {
            module_data.resolved_imports.push(ResolvedImport {
                module: target_module,
                function: target_function,
                arity: 1,
                target: ResolvedImportTarget::Native(native_entry),
            });
        }
        self.module_registry.insert(module_data);
    }
}

fn test_activity_answer(
    args: &[Term],
    _: &mut beamr::native::ProcessContext,
) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::small_int(0));
    }
    Ok(Term::small_int(42))
}

fn test_activity_fail(args: &[Term], _: &mut beamr::native::ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Ok(Term::NIL);
    }
    Err(Term::atom(beamr::atom::Atom::ERROR))
}
