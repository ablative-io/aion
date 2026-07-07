use std::collections::{HashMap, HashSet};

use crate::{
    ActionDecl, BinaryOp, CallTarget, Document, Expr, HandlerBlock, HandlerTerminal, IoDecl,
    RecordField, Span, Spanned, StepDecl, StepOp, TypeDecl, TypeRef,
};

/// A typechecker diagnostic with a source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    /// The source span for the offending expression or name.
    pub span: Span,
    /// Human-readable diagnostic text.
    pub message: String,
}

impl CheckError {
    fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ty {
    Bool,
    Int,
    Float,
    String,
    Nil,
    Dir,
    Duration,
    List(Box<Ty>),
    Option(Box<Ty>),
    Record(String),
    OpaqueChild,
    Unknown,
}

impl Ty {
    fn is_primitive_comparable(&self) -> bool {
        matches!(
            self,
            Self::Bool | Self::Int | Self::Float | Self::String | Self::Duration
        )
    }

    fn display(&self) -> String {
        match self {
            Self::Bool => "Bool".to_owned(),
            Self::Int => "Int".to_owned(),
            Self::Float => "Float".to_owned(),
            Self::String => "String".to_owned(),
            Self::Nil => "Nil".to_owned(),
            Self::Dir => "Dir".to_owned(),
            Self::Duration => "Duration".to_owned(),
            Self::List(inner) => format!("List({})", inner.display()),
            Self::Option(inner) => format!("Option({})", inner.display()),
            Self::Record(name) => name.clone(),
            Self::OpaqueChild => "untyped child result".to_owned(),
            Self::Unknown => "<unknown>".to_owned(),
        }
    }
}

#[derive(Debug)]
struct ActionSig {
    params: Vec<(String, Ty)>,
    returns: Ty,
}

#[derive(Debug)]
struct Ctx<'a> {
    types: HashMap<&'a str, &'a TypeDecl>,
    actions: HashMap<&'a str, ActionSig>,
    signals: HashMap<&'a str, Ty>,
    bindings: HashMap<String, Ty>,
    errors: Vec<CheckError>,
}

/// Typecheck a parsed AWL document. An empty vector means the document is well-typed.
pub fn check(document: &Document) -> Vec<CheckError> {
    let mut ctx = Ctx::new(document);
    ctx.check_document(document);
    ctx.errors
}

impl<'a> Ctx<'a> {
    fn new(document: &'a Document) -> Self {
        let mut ctx = Self {
            types: HashMap::new(),
            actions: HashMap::new(),
            signals: HashMap::new(),
            bindings: HashMap::new(),
            errors: Vec::new(),
        };
        ctx.collect_types(&document.types);
        ctx.collect_signals(&document.signals);
        ctx.collect_actions(&document.actions);
        ctx.collect_inputs(&document.inputs);
        ctx
    }

    fn check_document(&mut self, document: &Document) {
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

    fn collect_types(&mut self, types: &'a [TypeDecl]) {
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

    fn collect_signals(&mut self, signals: &'a [IoDecl]) {
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

    fn collect_actions(&mut self, actions: &'a [ActionDecl]) {
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

    fn collect_inputs(&mut self, inputs: &[IoDecl]) {
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

    fn check_step(&mut self, step: &StepDecl, output: &Ty) {
        self.check_value_name(&step.name, step.span, "step");
        if let Some(when) = &step.when {
            self.expect_expr(when, &Ty::Bool, "when guard");
        }
        if let Some(until) = &step.until {
            self.expect_expr(until, &Ty::Bool, "until guard");
        }
        if let Some(repeat) = &step.repeat {
            self.expect_expr(repeat, &Ty::Int, "repeat up to expression");
        }

        let mut step_binding = None;
        if let Some(each) = &step.each {
            self.check_value_name(&each.name, each.span, "each binding");
            let iter_ty = self.expr_ty(&each.in_expr);
            match iter_ty {
                Ty::List(inner) => step_binding = Some((each.name.clone(), *inner)),
                Ty::Unknown => {}
                found => self.error(
                    each.in_expr.span(),
                    format!(
                        "each expression expected List(T), found {}",
                        found.display()
                    ),
                ),
            }
        }

        let shadowed = step_binding
            .clone()
            .and_then(|(name, ty)| self.bindings.insert(name, ty));
        let result = self.step_op_ty(&step.op);
        if let Some((name, _)) = step_binding {
            if let Some(old) = shadowed {
                self.bindings.insert(name, old);
            } else {
                self.bindings.remove(&name);
            }
        }

        for handler in [&step.on_timeout, &step.on_failure].into_iter().flatten() {
            self.check_handler(handler, output);
        }

        if let Some(bind) = &step.bind_as {
            self.check_value_name(&bind.name, bind.span, "as binding");
            let bound = if step.each.is_some() && !matches!(result, Ty::Unknown | Ty::OpaqueChild) {
                Ty::List(Box::new(result))
            } else {
                result
            };
            if let Some(existing) = self.bindings.get(&bind.name) {
                if existing != &bound && !matches!(bound, Ty::Unknown) {
                    self.error(
                        bind.span,
                        format!(
                            "as binding `{}` expected {}, found {}",
                            bind.name,
                            existing.display(),
                            bound.display()
                        ),
                    );
                }
            } else {
                self.bindings.insert(bind.name.clone(), bound);
            }
        }
    }

    fn check_handler(&mut self, handler: &HandlerBlock, output: &Ty) {
        for action in &handler.actions {
            self.call_ty(action);
        }
        if let HandlerTerminal::Finish(expr) = &handler.terminal {
            self.expect_expr(expr, output, "handler finish expression");
        }
    }

    fn step_op_ty(&mut self, op: &StepOp) -> Ty {
        match op {
            StepOp::Do(target) => self.call_ty(target),
            StepOp::Wait { span, signal } => match self.signals.get(signal) {
                Some(ty) => ty.clone(),
                None => {
                    self.error(*span, format!("unknown signal `{signal}`"));
                    Ty::Unknown
                }
            },
            StepOp::Sleep(_) => Ty::Nil,
        }
    }

    fn call_ty(&mut self, target: &CallTarget) -> Ty {
        match target {
            CallTarget::Action(call) => {
                let Some(sig) = self.actions.get(call.name.as_str()) else {
                    self.error(call.span, format!("unknown action `{}`", call.name));
                    for arg in &call.args {
                        self.expr_ty(arg);
                    }
                    return Ty::Unknown;
                };
                let expected_len = sig.params.len();
                let returns = sig.returns.clone();
                let params: Vec<(String, Ty)> = sig
                    .params
                    .iter()
                    .map(|(name, ty)| (name.clone(), ty.clone()))
                    .collect();
                if call.args.len() != expected_len {
                    self.error(
                        call.span,
                        format!(
                            "action `{}` expected {} argument(s), found {}",
                            call.name,
                            expected_len,
                            call.args.len()
                        ),
                    );
                }
                for (arg, (name, expected)) in call.args.iter().zip(params.iter()) {
                    let found = self.expr_ty(arg);
                    self.expect_type(
                        arg.span(),
                        &found,
                        expected,
                        format!("argument `{name}` for action `{}`", call.name),
                    );
                }
                for arg in call.args.iter().skip(expected_len) {
                    self.expr_ty(arg);
                }
                returns
            }
            CallTarget::Child {
                span,
                workflow,
                args,
            } => {
                self.check_value_name(workflow, *span, "child workflow");
                for arg in args {
                    self.expr_ty(arg);
                }
                Ty::OpaqueChild
            }
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

    fn resolve_type_ref(ty: &TypeRef) -> Ty {
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

    fn expr_ty(&mut self, expr: &Expr) -> Ty {
        match expr {
            Expr::String { .. } => Ty::String,
            Expr::Int { .. } => Ty::Int,
            Expr::Float { .. } => Ty::Float,
            Expr::Bool { .. } => Ty::Bool,
            Expr::Duration(_) => Ty::Duration,
            Expr::List { span, items } => self.list_ty(*span, items),
            Expr::Ref { span, name } => {
                self.check_value_name(name, *span, "reference");
                match self.bindings.get(name) {
                    Some(ty) => ty.clone(),
                    None => {
                        self.error(*span, format!("unresolved reference `{name}`"));
                        Ty::Unknown
                    }
                }
            }
            Expr::Field { span, base, field } => self.field_ty(*span, base, field),
            Expr::Record { span, name, fields } => self.record_ty(*span, name, fields),
            Expr::Not { span, expr } => {
                let found = self.expr_ty(expr);
                self.expect_type(*span, &found, &Ty::Bool, "not operand");
                Ty::Bool
            }
            Expr::Binary {
                span,
                left,
                op,
                right,
            } => self.binary_ty(*span, left, *op, right),
        }
    }

    fn list_ty(&mut self, span: Span, items: &[Expr]) -> Ty {
        let Some((first, rest)) = items.split_first() else {
            self.error(span, "empty list literal has no inferable element type");
            return Ty::Unknown;
        };
        let element = self.expr_ty(first);
        for item in rest {
            let found = self.expr_ty(item);
            self.expect_type(item.span(), &found, &element, "list element");
        }
        Ty::List(Box::new(element))
    }

    fn field_ty(&mut self, span: Span, base: &Expr, field: &str) -> Ty {
        match self.expr_ty(base) {
            Ty::Record(name) => {
                let Some(decl) = self.types.get(name.as_str()) else {
                    self.error(span, format!("unknown type `{name}`"));
                    return Ty::Unknown;
                };
                let Some(found) = decl
                    .fields
                    .iter()
                    .find(|decl_field| decl_field.name == field)
                else {
                    self.error(span, format!("type `{name}` has no field `{field}`"));
                    return Ty::Unknown;
                };
                let ty = found.ty.clone();
                Self::resolve_type_ref(&ty)
            }
            Ty::OpaqueChild => {
                self.error(
                    span,
                    "child result is untyped in this revision and cannot be field-accessed",
                );
                Ty::Unknown
            }
            Ty::Unknown => Ty::Unknown,
            found => {
                self.error(
                    span,
                    format!(
                        "field access expected record type, found {}",
                        found.display()
                    ),
                );
                Ty::Unknown
            }
        }
    }

    fn record_ty(&mut self, span: Span, name: &str, fields: &[RecordField]) -> Ty {
        self.check_type_name(name, span);
        let Some(decl) = self.types.get(name) else {
            self.error(span, format!("unknown record type `{name}`"));
            for field in fields {
                self.expr_ty(&field.value);
            }
            return Ty::Unknown;
        };
        let field_refs: Vec<(String, TypeRef)> = decl
            .fields
            .iter()
            .map(|field| (field.name.clone(), field.ty.clone()))
            .collect();
        let decl_fields: Vec<(String, Ty)> = field_refs
            .iter()
            .map(|(field, ty)| (field.clone(), Self::resolve_type_ref(ty)))
            .collect();
        let mut seen = HashSet::new();
        for field in fields {
            self.check_value_name(&field.name, field.span, "record field");
            if !seen.insert(field.name.as_str()) {
                self.error(field.span, format!("duplicate field `{}`", field.name));
                self.expr_ty(&field.value);
                continue;
            }
            let Some((_, expected)) = decl_fields
                .iter()
                .find(|(decl_field, _)| decl_field == &field.name)
            else {
                self.error(
                    field.span,
                    format!("extra field `{}` for record `{name}`", field.name),
                );
                self.expr_ty(&field.value);
                continue;
            };
            let found = self.expr_ty(&field.value);
            self.expect_type(
                field.value.span(),
                &found,
                expected,
                format!("field `{}`", field.name),
            );
        }
        for (decl_field, _) in &decl_fields {
            if !seen.contains(decl_field.as_str()) {
                self.error(
                    span,
                    format!("missing field `{decl_field}` for record `{name}`"),
                );
            }
        }
        Ty::Record(name.to_owned())
    }

    fn binary_ty(&mut self, span: Span, left: &Expr, op: BinaryOp, right: &Expr) -> Ty {
        let left_ty = self.expr_ty(left);
        let right_ty = self.expr_ty(right);
        match op {
            BinaryOp::And | BinaryOp::Or => {
                self.expect_type(left.span(), &left_ty, &Ty::Bool, "left boolean operand");
                self.expect_type(right.span(), &right_ty, &Ty::Bool, "right boolean operand");
                Ty::Bool
            }
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => {
                if left_ty != Ty::Unknown
                    && right_ty != Ty::Unknown
                    && (left_ty != right_ty || !left_ty.is_primitive_comparable())
                {
                    self.error(
                        span,
                        format!(
                            "comparison expected matching primitive operands, found {} and {}",
                            left_ty.display(),
                            right_ty.display()
                        ),
                    );
                }
                Ty::Bool
            }
            BinaryOp::Add => {
                self.expect_type(left.span(), &left_ty, &Ty::String, "left + operand");
                self.expect_type(right.span(), &right_ty, &Ty::String, "right + operand");
                Ty::String
            }
        }
    }

    fn expect_expr(&mut self, expr: &Expr, expected: &Ty, context: &str) {
        let found = self.expr_ty(expr);
        self.expect_type(expr.span(), &found, expected, context);
    }

    fn expect_type(&mut self, span: Span, found: &Ty, expected: &Ty, context: impl AsRef<str>) {
        if found != expected && !matches!(found, Ty::Unknown) {
            self.error(
                span,
                format!(
                    "{} expected {}, found {}",
                    context.as_ref(),
                    expected.display(),
                    found.display()
                ),
            );
        }
    }

    fn check_value_name(&mut self, name: &str, span: Span, kind: &str) {
        if !is_snake_case(name) {
            self.error(
                span,
                format!("{kind} name `{name}` must be snake_case ([a-z][a-z0-9_]*)"),
            );
        }
    }

    fn check_type_name(&mut self, name: &str, span: Span) {
        if !is_title_case(name) {
            self.error(
                span,
                format!("type name `{name}` must be TitleCase ([A-Z][A-Za-z0-9]*)"),
            );
        }
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(CheckError::new(span, message));
    }
}

fn is_builtin(name: &str) -> bool {
    matches!(name, "Bool" | "Int" | "Float" | "String" | "Nil" | "Dir")
}

fn is_snake_case(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn is_title_case(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase() && chars.all(|ch| ch.is_ascii_alphanumeric())
}
