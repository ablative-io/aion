//! The module-level assembly pools (AWL-BC-IR.md §11.5 finalize): the atom
//! table, literal pool (deduped first-use), import table (used `RuntimeFn`
//! subset in first-use order, IR-24), `FunT` lambda table (`MakeClosure`
//! first-use), and the module-wide symbolic-label allocator.
//!
//! Labels are assigned deterministically: function `i` owns `entry = 2i+1` and
//! `body = 2i+2` (the shared dead-body function is an ordinary
//! `module.functions` entry and draws its labels the same way); extra
//! in-function labels (`Lexit`, `TryBind` fail) draw from a counter starting
//! after those, so the whole numbering is a pure function of the `MirModule`.

use std::collections::{HashMap, HashSet};

use beamr::atom::{Atom, AtomTable};
use beamr::loader::decode::{ImportEntry, LambdaEntry, Literal};

use crate::mir::runtime::RuntimeFn;
use crate::mir::{FnRef, MirLiteral, MirModule};

use super::error::SelectError;

/// The two labels bracketing a function: `entry` precedes `FuncInfo`, `body`
/// follows it (the erlc two-label shape; calls and exports target `body`).
#[derive(Debug, Clone, Copy)]
pub(super) struct FnLabels {
    pub(super) entry: u32,
    pub(super) body: u32,
}

pub(super) struct Builder<'m> {
    pub(super) module: &'m MirModule,
    atom_table: AtomTable,
    atoms: Vec<Atom>,
    atom_seen: HashSet<Atom>,
    literals: Vec<Literal>,
    imports: Vec<ImportEntry>,
    import_index: HashMap<RuntimeFn, usize>,
    lambdas: Vec<LambdaEntry>,
    /// Keyed by the target function's body label so repeated `MakeClosure`s of
    /// the same function share one `FunT` entry.
    lambda_index: HashMap<u32, usize>,
    next_label: u32,
}

impl<'m> Builder<'m> {
    pub(super) fn new(module: &'m MirModule) -> Self {
        let fn_count = u32::try_from(module.functions.len()).unwrap_or(u32::MAX);
        Self {
            module,
            atom_table: AtomTable::with_common_atoms(),
            atoms: Vec::new(),
            atom_seen: HashSet::new(),
            literals: Vec::new(),
            imports: Vec::new(),
            import_index: HashMap::new(),
            lambdas: Vec::new(),
            lambda_index: HashMap::new(),
            // First free label after every function's `2i+1 / 2i+2` pair
            // (indices `0..fn_count`, whose max label is `2 * fn_count`).
            next_label: 2 * fn_count + 1,
        }
    }

    // ---- labels ----

    pub(super) fn fn_labels(reference: FnRef) -> FnLabels {
        FnLabels {
            entry: 2 * reference.0 + 1,
            body: 2 * reference.0 + 2,
        }
    }

    pub(super) fn fresh_label(&mut self) -> u32 {
        let label = self.next_label;
        self.next_label += 1;
        label
    }

    // ---- atoms ----

    pub(super) fn atom(&mut self, name: &str) -> Atom {
        let atom = self.atom_table.intern(name);
        if self.atom_seen.insert(atom) {
            self.atoms.push(atom);
        }
        atom
    }

    /// Resolve a MIR `AtomRef` (index into the lower-produced atom table) to an
    /// interned, registered atom.
    pub(super) fn mir_atom(&mut self, index: u32) -> Result<Atom, SelectError> {
        let name = self
            .module
            .atom(index)
            .ok_or_else(|| SelectError::invariant(format!("atom ref {index} out of range")))?
            .to_owned();
        Ok(self.atom(&name))
    }

    // ---- literals ----

    pub(super) fn binary_literal(&mut self, bytes: Vec<u8>) -> usize {
        self.push_literal(Literal::Binary(bytes))
    }

    pub(super) fn mir_literal(
        &mut self,
        reference: crate::mir::LitRef,
    ) -> Result<usize, SelectError> {
        let literal = self
            .module
            .literals
            .get(reference.0 as usize)
            .ok_or_else(|| SelectError::invariant(format!("lit ref {} out of range", reference.0)))?
            .clone();
        let converted = self.convert_literal(&literal)?;
        Ok(self.push_literal(converted))
    }

    fn convert_literal(&mut self, literal: &MirLiteral) -> Result<Literal, SelectError> {
        Ok(match literal {
            MirLiteral::Integer(value) => Literal::Integer(*value),
            MirLiteral::Float { lexeme } => {
                let value = lexeme.parse::<f64>().map_err(|_| {
                    SelectError::invariant(format!("float lexeme `{lexeme}` does not parse"))
                })?;
                Literal::Float(value)
            }
            MirLiteral::Atom(reference) => Literal::Atom(self.mir_atom(reference.0)?),
            MirLiteral::Binary(bytes) => Literal::Binary(bytes.clone()),
            MirLiteral::Nil => Literal::Nil,
            MirLiteral::Tuple(elements) => Literal::Tuple(
                elements
                    .iter()
                    .map(|element| self.convert_literal(element))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            MirLiteral::List(elements) => {
                let converted = elements
                    .iter()
                    .map(|element| self.convert_literal(element))
                    .collect::<Result<Vec<_>, _>>()?;
                Literal::List(converted, Box::new(Literal::Nil))
            }
        })
    }

    fn push_literal(&mut self, literal: Literal) -> usize {
        if let Some(index) = self
            .literals
            .iter()
            .position(|existing| *existing == literal)
        {
            return index;
        }
        self.literals.push(literal);
        self.literals.len() - 1
    }

    // ---- imports ----

    pub(super) fn import(&mut self, callee: RuntimeFn) -> Result<usize, SelectError> {
        if let Some(index) = self.import_index.get(&callee) {
            return Ok(*index);
        }
        let (module_name, function_name, arity) = callee.signature();
        let module_atom = self.atom(module_name);
        let function_atom = self.atom(&function_name);
        let arity = u8::try_from(arity).map_err(|_| SelectError::OutOfRange {
            what: format!("import arity for {callee:?}"),
        })?;
        let index = self.imports.len();
        self.imports.push(ImportEntry {
            module: module_atom,
            function: function_atom,
            arity,
        });
        self.import_index.insert(callee, index);
        Ok(index)
    }

    // ---- lambdas (FunT) ----

    /// Intern (or reuse) a `FunT` entry for a `make_fun2` of the function at
    /// `body_label`. `num_free` is the physical capture count (S9).
    pub(super) fn lambda(
        &mut self,
        name: Atom,
        arity: u8,
        body_label: u32,
        num_free: u32,
    ) -> usize {
        if let Some(index) = self.lambda_index.get(&body_label) {
            return *index;
        }
        let index = self.lambdas.len();
        self.lambdas.push(LambdaEntry {
            function: name,
            arity,
            label: body_label,
            num_free,
            unique_id: 0,
        });
        self.lambda_index.insert(body_label, index);
        index
    }

    /// Consume the pools into the parts an assembled `ParsedModule` needs.
    pub(super) fn into_parts(self) -> BuilderParts {
        BuilderParts {
            atom_table: self.atom_table,
            atoms: self.atoms,
            literals: self.literals,
            imports: self.imports,
            lambdas: self.lambdas,
        }
    }
}

pub(super) struct BuilderParts {
    pub(super) atom_table: AtomTable,
    pub(super) atoms: Vec<Atom>,
    pub(super) literals: Vec<Literal>,
    pub(super) imports: Vec<ImportEntry>,
    pub(super) lambdas: Vec<LambdaEntry>,
}
