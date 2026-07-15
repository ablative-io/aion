//! The shared checking context: declaration tables built by the declaration
//! pass and consumed by the graph and flow passes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Span;
use crate::ast::{Document, Step, SubflowDecl, TypeRef};
use crate::semantic::{Builder, DeclarationKind};

use super::error::CheckError;
use super::types::{Ty, TypeTable};

/// One flow under graph/walk analysis: the workflow's top-level steps, or
/// one subflow's. A subflow has the workflow's anatomy — its parameters are
/// its inputs and its single outcome is the one route-targetable exit — so
/// every step-level pass runs once per flow over this view.
pub(super) struct Flow<'a> {
    /// The flow's steps in document order.
    pub(super) steps: &'a [Step],
    /// The flow's inputs: workflow inputs, or the subflow's parameters.
    pub(super) inputs: BTreeMap<String, Ty>,
    /// Input declaration spans in order, for origin tracking.
    pub(super) input_origins: Vec<(String, Span)>,
    /// Route-targetable outcomes of this flow.
    pub(super) outcomes: BTreeMap<String, Ty>,
    /// `None` for the workflow's own steps; the subflow's name otherwise.
    pub(super) subflow: Option<String>,
}

impl<'a> Flow<'a> {
    /// The workflow's own flow view.
    pub(super) fn workflow(ctx: &Ctx<'a>) -> Self {
        Self {
            steps: &ctx.doc.steps,
            inputs: ctx.inputs.clone(),
            input_origins: ctx
                .doc
                .inputs
                .iter()
                .map(|input| (input.name.clone(), input.name_span))
                .collect(),
            outcomes: ctx.outcome_types.clone(),
            subflow: None,
        }
    }

    /// One subflow's flow view: parameters as inputs, its single outcome as
    /// the only route-targetable exit.
    pub(super) fn subflow(ctx: &Ctx<'_>, decl: &'a SubflowDecl) -> Self {
        let mut inputs = BTreeMap::new();
        let mut input_origins = Vec::new();
        let mut outcomes = BTreeMap::new();
        if let Some(info) = ctx.subflows.get(&decl.name) {
            for param in &info.params {
                inputs.insert(param.name.clone(), param.ty.clone());
            }
            outcomes.insert(decl.outcome.name.clone(), info.returns.clone());
        }
        for param in &decl.params {
            input_origins.push((param.name.clone(), param.name_span));
        }
        Self {
            steps: &decl.steps,
            inputs,
            input_origins,
            outcomes,
            subflow: Some(decl.name.clone()),
        }
    }
}

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

/// One checked document-level `const`: its folded value type and the span of
/// its declared name (for semantic references).
#[derive(Debug, Clone)]
pub(super) struct ConstInfo {
    /// The folded value's type.
    pub(super) ty: Ty,
    /// Source span of the const's declared name.
    pub(super) name_span: crate::Span,
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
    /// Subflows by name: parameters and the single outcome's payload type.
    pub(super) subflows: BTreeMap<String, Callable>,
    /// Document-level consts: name → folded type and declaration site.
    pub(super) consts: BTreeMap<String, ConstInfo>,
    /// Workflow inputs: name → declared type.
    pub(super) inputs: BTreeMap<String, Ty>,
    /// Declared signals: name → payload type.
    pub(super) signals: BTreeMap<String, Ty>,
    /// Workflow outcomes: name → payload type.
    pub(super) outcome_types: BTreeMap<String, Ty>,
    /// Semantic facts retained from checker computations.
    pub(super) semantic: Builder,
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
            subflows: BTreeMap::new(),
            consts: BTreeMap::new(),
            inputs: BTreeMap::new(),
            signals: BTreeMap::new(),
            outcome_types: BTreeMap::new(),
            semantic: Builder::new(doc),
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
        let ty = match type_ref {
            TypeRef::Named { span, name } => match name.as_str() {
                "Bool" => Ty::Bool,
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "String" => Ty::Str,
                "Nil" => Ty::Nil,
                "Dir" => Ty::Dir,
                other if self.type_names.contains(other) => {
                    self.semantic.reference(*span, DeclarationKind::Type, other);
                    Ty::Named(other.to_owned())
                }
                other => {
                    self.error(*span, format!("unknown type `{other}`"));
                    Ty::Unknown
                }
            },
            TypeRef::List { inner, .. } => {
                if let TypeRef::Optional {
                    span,
                    inner: element,
                } = inner.as_ref()
                {
                    let element_ty = self.resolve_type_ref(element);
                    self.error(
                        *span,
                        format!(
                            "a list cannot have optional elements — an absent element is \
                             simply not in the list, and schema derivation cannot express \
                             element optionality; use [{element_ty}] (or [{element_ty}]? \
                             if the whole list may be absent)"
                        ),
                    );
                    Ty::List(std::rc::Rc::new(element_ty))
                } else {
                    let element = self.resolve_type_ref(inner);
                    Ty::List(std::rc::Rc::new(element))
                }
            }
            TypeRef::Optional { inner, .. } => self.resolve_type_ref(inner).optional(),
        };
        let span = match type_ref {
            TypeRef::Named { span, .. }
            | TypeRef::List { span, .. }
            | TypeRef::Optional { span, .. } => *span,
        };
        self.semantic.ty(span, &ty.to_string());
        ty
    }

    /// Look up a callable: worker actions first, then children.
    pub(super) fn callable(&self, name: &str) -> Option<&Callable> {
        self.actions.get(name).or_else(|| self.children.get(name))
    }
}
