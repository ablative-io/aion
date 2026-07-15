//! The lowering context: atom/literal interning, the `GType -> TyDesc` and
//! `GType -> WireDesc` maps, leaf detection, and per-function var allocation.

use crate::emitter::{Emitter, GType, NamedDef, Plan};

use super::super::func::MirFn;
use super::super::ids::{AtomRef, FnRef, LitRef, Var};
use super::super::shapes::{MirLiteral, WireDesc};
use super::super::tydesc::{Leaf, TyDesc};
use super::driver::LowerError;

/// Shared lowering state threaded through the build.
pub(super) struct Ctx<'a> {
    pub(super) emitter: &'a Emitter<'a>,
    pub(super) plan: &'a Plan,
    pub(super) module_name: String,
    pub(super) atoms: Vec<String>,
    pub(super) literals: Vec<MirLiteral>,
    var_counter: u32,
    predicate_start: Option<FnRef>,
    predicates: Vec<Option<MirFn>>,
}

impl<'a> Ctx<'a> {
    pub(super) fn new(emitter: &'a Emitter<'a>, plan: &'a Plan, module_name: String) -> Self {
        Self {
            emitter,
            plan,
            module_name,
            atoms: Vec::new(),
            literals: Vec::new(),
            var_counter: 0,
            predicate_start: None,
            predicates: Vec::new(),
        }
    }

    /// Intern an atom, returning its stable index.
    pub(super) fn atom(&mut self, name: &str) -> AtomRef {
        if let Some(index) = self.atoms.iter().position(|existing| existing == name) {
            return AtomRef(u32::try_from(index).unwrap_or(u32::MAX));
        }
        let index = u32::try_from(self.atoms.len()).unwrap_or(u32::MAX);
        self.atoms.push(name.to_owned());
        AtomRef(index)
    }

    /// Add a binary literal (a UTF-8 string), returning its pool index.
    pub(super) fn binary(&mut self, value: &str) -> LitRef {
        self.push_literal(MirLiteral::Binary(value.as_bytes().to_vec()))
    }

    /// Add a float literal, retaining the source lexeme (S3).
    pub(super) fn push_float(&mut self, lexeme: &str) -> LitRef {
        self.push_literal(MirLiteral::Float {
            lexeme: lexeme.to_owned(),
        })
    }

    fn push_literal(&mut self, literal: MirLiteral) -> LitRef {
        let index = u32::try_from(self.literals.len()).unwrap_or(u32::MAX);
        self.literals.push(literal);
        LitRef(index)
    }

    /// Reset the per-function var counter and return a fresh var.
    pub(super) fn reset_vars(&mut self) {
        self.var_counter = 0;
    }

    /// Swap the var counter (loop functions are their own SSA namespace,
    /// built mid-way through the host function's lowering: save the host's
    /// counter, lower the loop body from `v0`, restore).
    pub(super) fn swap_var_counter(&mut self, value: u32) -> u32 {
        std::mem::replace(&mut self.var_counter, value)
    }

    pub(super) fn fresh_var(&mut self) -> Var {
        let var = Var(self.var_counter);
        self.var_counter = self.var_counter.saturating_add(1);
        var
    }

    pub(super) fn set_predicate_start(&mut self, start: FnRef) {
        self.predicate_start = Some(start);
    }

    pub(super) fn take_predicate(&mut self) -> Result<(usize, FnRef), LowerError> {
        let ordinal = self.predicates.len();
        let start = self.predicate_start.ok_or_else(|| LowerError::Planning {
            message: "predicate function base was not initialized".to_owned(),
        })?;
        let offset = u32::try_from(ordinal).map_err(|_| LowerError::Planning {
            message: "too many predicate functions".to_owned(),
        })?;
        self.predicates.push(None);
        Ok((ordinal, FnRef(start.0.saturating_add(offset))))
    }

    pub(super) fn finish_predicate(&mut self, ordinal: usize, function: MirFn) {
        if let Some(slot) = self.predicates.get_mut(ordinal) {
            *slot = Some(function);
        }
    }

    pub(super) fn take_predicates(&mut self) -> Result<Vec<MirFn>, LowerError> {
        let mut completed = Vec::with_capacity(self.predicates.len());
        for (ordinal, slot) in std::mem::take(&mut self.predicates).into_iter().enumerate() {
            let function = slot.ok_or_else(|| LowerError::Planning {
                message: format!("predicate function slot {ordinal} was not completed"),
            })?;
            completed.push(function);
        }
        Ok(completed)
    }

    /// The leaf kind of a resolved leaf type.
    pub(super) fn leaf_of(&self, ty: &GType) -> Option<Leaf> {
        match self.emitter.env.resolve(ty) {
            GType::Bool => Some(Leaf::Bool),
            GType::Int => Some(Leaf::Int),
            GType::Float => Some(Leaf::Float),
            GType::Str => Some(Leaf::Str),
            GType::Nil | GType::Unknown => Some(Leaf::Nil),
            _ => None,
        }
    }

    /// The codec-function stem for a wire type (`x` in `x_codec`).
    pub(super) fn codec_stem(&self, ty: &GType) -> String {
        self.emitter.env.codec_name(ty)
    }

    /// The total `GType -> TyDesc` mapping (§5). Named records/enums/unions
    /// belong to the current module.
    pub(super) fn tydesc(&self, ty: &GType) -> TyDesc {
        match ty {
            GType::Bool => TyDesc::Bool,
            GType::Int => TyDesc::Int,
            GType::Float => TyDesc::Float,
            GType::Str => TyDesc::String,
            GType::Nil => TyDesc::Nil,
            GType::Unknown => TyDesc::Unknown,
            GType::Duration => TyDesc::Duration,
            GType::List(inner) => TyDesc::List(Box::new(self.tydesc(inner))),
            GType::Option(inner) => TyDesc::Option(Box::new(self.tydesc(inner))),
            GType::Named(name) => match self.emitter.env.get(name) {
                Some(NamedDef::Alias(inner)) => self.tydesc(&inner.clone()),
                _ => TyDesc::Custom {
                    module: self.module_name.clone(),
                    name: name.clone(),
                    params: Vec::new(),
                },
            },
        }
    }

    /// The `GType -> WireDesc` mapping for codec-template parameters (§3). Named
    /// types become `Ref`, resolved through aliases first.
    pub(super) fn wiredesc(&self, ty: &GType) -> WireDesc {
        match ty {
            GType::Bool => WireDesc::Bool,
            GType::Int => WireDesc::Int,
            GType::Float => WireDesc::Float,
            GType::Str => WireDesc::Str,
            GType::Nil | GType::Unknown | GType::Duration => WireDesc::Nil,
            GType::List(inner) => WireDesc::List(Box::new(self.wiredesc(inner))),
            GType::Option(inner) => WireDesc::Nullable(Box::new(self.wiredesc(inner))),
            GType::Named(name) => match self.emitter.env.get(name) {
                Some(NamedDef::Alias(inner)) => self.wiredesc(&inner.clone()),
                _ => WireDesc::Ref(name.clone()),
            },
        }
    }
}
