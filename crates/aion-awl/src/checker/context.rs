//! The shared checking context: declaration tables built by the declaration
//! pass and consumed by the graph and flow passes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Span;
use crate::ast::{Document, TypeRef};

use super::error::CheckError;
use super::types::{Ty, TypeTable};

/// A callable contract: a worker action or a child workflow.
#[derive(Debug, Clone)]
pub(super) struct Callable {
    /// Declared parameters in order.
    pub(super) params: Vec<Param>,
    /// Declared result type.
    pub(super) returns: Ty,
}

/// One declared parameter of a callable.
#[derive(Debug, Clone)]
pub(super) struct Param {
    /// Parameter name.
    pub(super) name: String,
    /// Parameter type.
    pub(super) ty: Ty,
}

/// The checking context threaded through every pass.
pub(super) struct Ctx<'a> {
    /// The document being checked.
    pub(super) doc: &'a Document,
    /// Directory schema imports resolve against (the document's directory).
    pub(super) root: Option<&'a Path>,
    /// Names of every declared type (registered before bodies resolve).
    pub(super) type_names: BTreeSet<String>,
    /// Declared type definitions (records, enums, projected schema doors).
    pub(super) types: TypeTable,
    /// Worker actions by name.
    pub(super) actions: BTreeMap<String, Callable>,
    /// Child workflows by name.
    pub(super) children: BTreeMap<String, Callable>,
    /// Workflow inputs: name → declared type.
    pub(super) inputs: BTreeMap<String, Ty>,
    /// Declared signals: name → payload type.
    pub(super) signals: BTreeMap<String, Ty>,
    /// Workflow outcomes: name → payload type.
    pub(super) outcome_types: BTreeMap<String, Ty>,
    /// Accumulated diagnostics.
    pub(super) errors: Vec<CheckError>,
}

/// Builtin type names, reserved against redeclaration.
pub(super) const BUILTIN_TYPES: [&str; 6] = ["Bool", "Int", "Float", "String", "Nil", "Dir"];

impl<'a> Ctx<'a> {
    pub(super) fn new(doc: &'a Document, root: Option<&'a Path>) -> Self {
        Self {
            doc,
            root,
            type_names: BTreeSet::new(),
            types: TypeTable::new(),
            actions: BTreeMap::new(),
            children: BTreeMap::new(),
            inputs: BTreeMap::new(),
            signals: BTreeMap::new(),
            outcome_types: BTreeMap::new(),
            errors: Vec::new(),
        }
    }

    /// Record a diagnostic.
    pub(super) fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(CheckError::new(span, message));
    }

    /// Resolve a syntactic type reference to a semantic type, reporting
    /// unknown type names.
    pub(super) fn resolve_type_ref(&mut self, type_ref: &TypeRef) -> Ty {
        match type_ref {
            TypeRef::Named { span, name } => match name.as_str() {
                "Bool" => Ty::Bool,
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "String" => Ty::Str,
                "Nil" => Ty::Nil,
                "Dir" => Ty::Dir,
                other if self.type_names.contains(other) => Ty::Named(other.to_owned()),
                other => {
                    self.error(*span, format!("unknown type `{other}`"));
                    Ty::Unknown
                }
            },
            TypeRef::List { inner, .. } => {
                let element = self.resolve_type_ref(inner);
                Ty::List(std::rc::Rc::new(element))
            }
            TypeRef::Optional { inner, .. } => self.resolve_type_ref(inner).optional(),
        }
    }

    /// Look up a callable: worker actions first, then children.
    pub(super) fn callable(&self, name: &str) -> Option<&Callable> {
        self.actions.get(name).or_else(|| self.children.get(name))
    }
}
