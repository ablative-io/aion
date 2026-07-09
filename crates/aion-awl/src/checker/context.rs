use std::collections::HashMap;

use crate::{Document, Expr, Span, Spanned, TypeDecl};

use super::{
    error::CheckError,
    types::{ActionSig, Ty},
};

pub(super) struct Ctx<'a> {
    pub(super) types: HashMap<&'a str, &'a TypeDecl>,
    pub(super) actions: HashMap<&'a str, ActionSig>,
    pub(super) signals: HashMap<&'a str, Ty>,
    pub(super) bindings: HashMap<String, Ty>,
    pub(super) errors: Vec<CheckError>,
}

/// Typecheck a parsed AWL document. An empty vector means the document is well-typed.
#[must_use]
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

    pub(super) fn expect_expr(&mut self, expr: &Expr, expected: &Ty, context: &str) {
        let found = self.expr_ty(expr);
        self.expect_type(expr.span(), &found, expected, context);
    }

    pub(super) fn expect_type(
        &mut self,
        span: Span,
        found: &Ty,
        expected: &Ty,
        context: impl AsRef<str>,
    ) {
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

    pub(super) fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(CheckError::new(span, message));
    }
}
