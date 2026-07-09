use std::collections::HashSet;

use crate::{ActionDecl, Document, IoDecl, TypeDecl, TypeRef};

use super::{
    context::Ctx,
    idents::{is_builtin, is_snake_case, is_title_case},
    types::{ActionSig, Ty},
};

impl<'a> Ctx<'a> {
    pub(super) fn check_document(&mut self, document: &Document) {
        self.check_value_name(&document.workflow.name, document.workflow.span, "workflow");
        self.check_type_refs(document);
        let output = document
            .output
            .as_ref()
            .map_or(Ty::Nil, |decl| Self::resolve_type_ref(&decl.ty));
        for step in &document.steps {
            self.check_step(step, &output);
        }
        self.expect_expr(&document.finish, &output, "finish expression");
    }

    pub(super) fn collect_types(&mut self, types: &'a [TypeDecl]) {
        for decl in types {
            self.check_type_name(&decl.name, decl.span);
            if self.types.insert(decl.name.as_str(), decl).is_some() {
                self.error(
                    decl.span,
                    format!("duplicate type declaration `{}`", decl.name),
                );
            }
            let mut fields = HashSet::new();
            for field in &decl.fields {
                self.check_value_name(&field.name, field.span, "field");
                if !fields.insert(field.name.as_str()) {
                    self.error(
                        field.span,
                        format!("duplicate field `{}` in type `{}`", field.name, decl.name),
                    );
                }
            }
        }
    }

    pub(super) fn collect_signals(&mut self, signals: &'a [IoDecl]) {
        for signal in signals {
            self.check_value_name(&signal.name, signal.span, "signal");
            let ty = Self::resolve_type_ref(&signal.ty);
            if self.signals.insert(signal.name.as_str(), ty).is_some() {
                self.error(
                    signal.span,
                    format!("duplicate signal declaration `{}`", signal.name),
                );
            }
        }
    }

    pub(super) fn collect_actions(&mut self, actions: &'a [ActionDecl]) {
        for action in actions {
            self.check_value_name(&action.name, action.span, "action");
            let params = action
                .params
                .iter()
                .map(|param| {
                    self.check_value_name(&param.name, param.span, "parameter");
                    (param.name.clone(), Self::resolve_type_ref(&param.ty))
                })
                .collect();
            let sig = ActionSig {
                params,
                returns: Self::resolve_type_ref(&action.returns),
            };
            if self.actions.insert(action.name.as_str(), sig).is_some() {
                self.error(
                    action.span,
                    format!("duplicate action declaration `{}`", action.name),
                );
            }
        }
    }

    pub(super) fn collect_inputs(&mut self, inputs: &[IoDecl]) {
        for input in inputs {
            self.check_value_name(&input.name, input.span, "input");
            let ty = Self::resolve_type_ref(&input.ty);
            if self.bindings.insert(input.name.clone(), ty).is_some() {
                self.error(
                    input.span,
                    format!("duplicate input declaration `{}`", input.name),
                );
            }
        }
    }

    fn check_type_refs(&mut self, document: &Document) {
        if let Some(output) = &document.output {
            self.check_type_ref(&output.ty);
        }
        if let Some(error) = &document.error {
            if !error.name.is_empty() {
                self.check_value_name(&error.name, error.span, "error");
            }
            self.check_type_ref(&error.ty);
        }
        for input in &document.inputs {
            self.check_type_ref(&input.ty);
        }
        for signal in &document.signals {
            self.check_type_ref(&signal.ty);
        }
        for ty in &document.types {
            for field in &ty.fields {
                self.check_type_ref(&field.ty);
            }
        }
        for action in &document.actions {
            for param in &action.params {
                self.check_type_ref(&param.ty);
            }
            self.check_type_ref(&action.returns);
        }
    }

    fn check_type_ref(&mut self, ty: &TypeRef) {
        match ty {
            TypeRef::Named { span, name } => {
                self.check_type_name(name, *span);
                if !is_builtin(name) && !self.types.contains_key(name.as_str()) {
                    self.error(*span, format!("unknown type `{name}`"));
                }
            }
            TypeRef::List { inner, .. } | TypeRef::Option { inner, .. } => {
                self.check_type_ref(inner);
            }
        }
    }

    pub(super) fn resolve_type_ref(ty: &TypeRef) -> Ty {
        match ty {
            TypeRef::Named { name, .. } => match name.as_str() {
                "Bool" => Ty::Bool,
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "String" => Ty::String,
                "Nil" => Ty::Nil,
                "Dir" => Ty::Dir,
                _ => Ty::Record(name.clone()),
            },
            TypeRef::List { inner, .. } => Ty::List(Box::new(Self::resolve_type_ref(inner))),
            TypeRef::Option { inner, .. } => Ty::Option(Box::new(Self::resolve_type_ref(inner))),
        }
    }

    pub(super) fn check_value_name(&mut self, name: &str, span: crate::Span, kind: &str) {
        if !is_snake_case(name) {
            self.error(
                span,
                format!("{kind} name `{name}` must be snake_case ([a-z][a-z0-9_]*)"),
            );
        }
    }

    pub(super) fn check_type_name(&mut self, name: &str, span: crate::Span) {
        if !is_title_case(name) {
            self.error(
                span,
                format!("type name `{name}` must be TitleCase ([A-Z][A-Za-z0-9]*)"),
            );
        }
    }
}
