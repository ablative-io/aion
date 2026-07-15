//! Compile-time folding of the B1 ergonomics vocabulary, run after a clean
//! typecheck and before either lowering backend: raw strings become plain
//! string literals, `json { … }` bodies become their verbatim `String`
//! values, `schema of Type` becomes the derived JSON Schema text, and const
//! references are replaced by their fully folded values — so the existing
//! lowerings only ever see the expression vocabulary they already speak.

use std::collections::BTreeMap;
use std::path::Path;

use crate::Span;
use crate::ast::{
    BinaryOp, ConstDecl, Document, Expr, ForkHeader, PipeEnd, PipeStage, RouteTarget, Statement,
    Step,
};
use crate::spanned::Spanned;

/// A fold failure. The checker rejects every document that could produce
/// one, so reaching this from `check`-gated paths indicates a defect
/// upstream; the error is still reported honestly, never panicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FoldError {
    /// Source span of the offending construct.
    pub(crate) span: Span,
    /// Human-readable diagnostic text.
    pub(crate) message: String,
}

/// The shared fold state: resolved const values and the document/root the
/// schema derivation reads.
struct Folder<'a> {
    document: &'a Document,
    root: Option<&'a Path>,
    /// Fully folded const values (literal-only expression trees), memoized.
    values: BTreeMap<String, Expr>,
    /// Consts currently being resolved, for cycle refusal.
    visiting: Vec<String>,
}

/// Fold `document` into an equivalent document containing only the
/// expression vocabulary the existing lowerings speak. A document using
/// none of the B1 constructs folds to an identical tree.
///
/// # Errors
///
/// Returns [`FoldError`] for const cycles, non-compile-time const values,
/// and schema derivations that fail — all shapes the checker already
/// rejects.
pub(crate) fn fold_document(
    document: &Document,
    root: Option<&Path>,
) -> Result<Document, FoldError> {
    let mut folder = Folder {
        document,
        root,
        values: BTreeMap::new(),
        visiting: Vec::new(),
    };
    for decl in &document.consts {
        folder.resolve(decl)?;
    }
    let mut rewritten = document.clone();
    for step in &mut rewritten.steps {
        folder.fold_step(step)?;
    }
    for subflow in &mut rewritten.subflows {
        for step in &mut subflow.steps {
            folder.fold_step(step)?;
        }
    }
    Ok(rewritten)
}

impl Folder<'_> {
    /// Resolve one const declaration to its folded value, memoized by name
    /// (first declaration wins under duplicates, mirroring the checker).
    fn resolve(&mut self, decl: &ConstDecl) -> Result<(), FoldError> {
        if self.values.contains_key(&decl.name) {
            return Ok(());
        }
        self.visiting.push(decl.name.clone());
        let value = self.eval(&decl.value);
        self.visiting.pop();
        let value = value?;
        self.values.entry(decl.name.clone()).or_insert(value);
        Ok(())
    }

    /// Evaluate a const value expression down to a literal-only tree.
    fn eval(&mut self, expr: &Expr) -> Result<Expr, FoldError> {
        match expr {
            Expr::String { .. }
            | Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Bool { .. }
            | Expr::Duration(_) => Ok(expr.clone()),
            Expr::RawString { span, value } => Ok(Expr::String {
                span: *span,
                value: value.clone(),
            }),
            Expr::Json { span, body, .. } => Ok(Expr::String {
                span: *span,
                value: body.clone(),
            }),
            Expr::SchemaOf {
                span,
                name,
                name_span,
            } => Ok(Expr::String {
                span: *span,
                value: self.schema_string(name, *name_span)?,
            }),
            Expr::List { span, items } => {
                let items = items
                    .iter()
                    .map(|item| self.eval(item))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Expr::List { span: *span, items })
            }
            Expr::Binary {
                span,
                left,
                op: BinaryOp::Concat,
                right,
            } => {
                let left = self.eval(left)?;
                let right = self.eval(right)?;
                match (left, right) {
                    (Expr::String { value: a, .. }, Expr::String { value: b, .. }) => {
                        Ok(Expr::String {
                            span: *span,
                            value: format!("{a}{b}"),
                        })
                    }
                    _ => Err(FoldError {
                        span: *span,
                        message: "`+` in a `const` joins strings only".to_owned(),
                    }),
                }
            }
            Expr::Ref { span, name } => self.const_value(name, *span),
            other => Err(FoldError {
                span: other.span(),
                message: "a `const` value must be compile-time".to_owned(),
            }),
        }
    }

    /// The folded value of the named const, re-spanned to the reference
    /// site so downstream diagnostics point at the use.
    fn const_value(&mut self, name: &str, span: Span) -> Result<Expr, FoldError> {
        if self.visiting.iter().any(|active| active == name) {
            return Err(FoldError {
                span,
                message: format!("const `{name}` is defined in terms of itself"),
            });
        }
        if !self.values.contains_key(name) {
            let Some(decl) = self.document.consts.iter().find(|decl| decl.name == name) else {
                return Err(FoldError {
                    span,
                    message: format!("unknown const `{name}`"),
                });
            };
            let decl = decl.clone();
            self.resolve(&decl)?;
        }
        let Some(value) = self.values.get(name) else {
            return Err(FoldError {
                span,
                message: format!("unknown const `{name}`"),
            });
        };
        Ok(with_span(value.clone(), span))
    }

    /// The compact JSON text of the named type's derived schema.
    fn schema_string(&self, name: &str, span: Span) -> Result<String, FoldError> {
        let derived = match self.root {
            Some(root) => crate::schema::schema_for_type_in(self.document, root, name),
            None => crate::schema::schema_for_type(self.document, name),
        }
        .map_err(|error| FoldError {
            span,
            message: format!("cannot derive `schema of {name}`: {error}"),
        })?;
        serde_json::to_string(&derived).map_err(|error| FoldError {
            span,
            message: format!("cannot serialize `schema of {name}`: {error}"),
        })
    }

    fn fold_step(&mut self, step: &mut Step) -> Result<(), FoldError> {
        self.fold_statements(&mut step.body)?;
        if let Some(on_failure) = &mut step.on_failure {
            self.fold_statements(&mut on_failure.body)?;
        }
        for clause in &mut step.outcomes {
            if let crate::ast::Guard::When { expr, .. } = &mut clause.guard {
                self.fold_expr(expr)?;
            }
            self.fold_route(&mut clause.route)?;
        }
        if let Some(max_visits) = &mut step.max_visits {
            self.fold_expr(&mut max_visits.bound)?;
        }
        Ok(())
    }

    fn fold_statements(&mut self, statements: &mut [Statement]) -> Result<(), FoldError> {
        for statement in statements {
            match statement {
                Statement::Call(call) => {
                    for arg in &mut call.call.args {
                        self.fold_expr(&mut arg.value)?;
                    }
                }
                Statement::Spawn(spawn) => {
                    for arg in &mut spawn.call.args {
                        self.fold_expr(&mut arg.value)?;
                    }
                }
                Statement::Pipe(pipe) => {
                    self.fold_expr(&mut pipe.head)?;
                    for stage in &mut pipe.stages {
                        if let PipeStage::Combinator(combinator) = stage
                            && let Some(arg) = &mut combinator.arg
                        {
                            self.fold_expr(arg)?;
                        }
                    }
                    if let PipeEnd::Route(target) = &mut pipe.end {
                        self.fold_route(target)?;
                    }
                }
                Statement::Fork(fork) => {
                    if let ForkHeader::Collection { collection, .. } = &mut fork.header {
                        self.fold_expr(collection)?;
                    }
                    self.fold_statements(&mut fork.body)?;
                }
                Statement::Loop(looped) => {
                    self.fold_expr(&mut looped.seed)?;
                    self.fold_statements(&mut looped.body)?;
                    if let Some(until) = &mut looped.until {
                        self.fold_expr(&mut until.expr)?;
                    }
                    if let Some(max) = &mut looped.max {
                        self.fold_expr(&mut max.expr)?;
                    }
                }
                Statement::Route(route) => self.fold_route(&mut route.target)?,
                Statement::SubStep(sub) => self.fold_step(sub)?,
                Statement::Distribute(distribute) => self.fold_expr(&mut distribute.collection)?,
                Statement::Wait(_) | Statement::Sleep(_) | Statement::Collect(_) => {}
            }
        }
        Ok(())
    }

    fn fold_route(&mut self, target: &mut RouteTarget) -> Result<(), FoldError> {
        match &mut target.payload {
            Some(crate::ast::RoutePayload::Args(args)) => {
                for arg in args {
                    self.fold_expr(&mut arg.value)?;
                }
            }
            Some(crate::ast::RoutePayload::Value(value)) => self.fold_expr(value)?,
            None => {}
        }
        Ok(())
    }

    /// Fold one general expression in place: substitute const references,
    /// convert the B1 literal forms, and recurse into composites. Runtime
    /// structure (concatenation of runtime values, field access, predicates)
    /// is left exactly as written.
    fn fold_expr(&mut self, expr: &mut Expr) -> Result<(), FoldError> {
        match expr {
            Expr::RawString { span, value } => {
                *expr = Expr::String {
                    span: *span,
                    value: std::mem::take(value),
                };
            }
            Expr::Json { span, body, .. } => {
                *expr = Expr::String {
                    span: *span,
                    value: std::mem::take(body),
                };
            }
            Expr::SchemaOf {
                span,
                name,
                name_span,
            } => {
                let value = self.schema_string(name, *name_span)?;
                *expr = Expr::String { span: *span, value };
            }
            Expr::Ref { span, name } => {
                if self.values.contains_key(name.as_str()) {
                    *expr = self.const_value(name, *span)?;
                }
            }
            Expr::List { items, .. } => {
                for item in items {
                    self.fold_expr(item)?;
                }
            }
            Expr::Record { args, .. } => {
                for arg in args {
                    self.fold_expr(&mut arg.value)?;
                }
            }
            Expr::Field { base, .. } | Expr::Index { base, .. } => self.fold_expr(base)?,
            Expr::Not { expr: inner, .. } => self.fold_expr(inner)?,
            Expr::Binary { left, right, .. } => {
                self.fold_expr(left)?;
                self.fold_expr(right)?;
            }
            Expr::Predicate { subject, .. } => self.fold_expr(subject)?,
            Expr::CollectionPredicate {
                collection,
                predicate,
                ..
            } => {
                self.fold_expr(collection)?;
                self.fold_expr(predicate)?;
            }
            Expr::String { .. }
            | Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Bool { .. }
            | Expr::Duration(_)
            | Expr::Workflow { .. }
            | Expr::Variant { .. }
            | Expr::Accessor { .. } => {}
        }
        Ok(())
    }
}

/// The expression with its top-level span replaced by `span` (nested spans
/// keep pointing at the const declaration's value, which is where the text
/// truly lives).
fn with_span(mut expr: Expr, span: Span) -> Expr {
    match &mut expr {
        Expr::String { span: at, .. }
        | Expr::RawString { span: at, .. }
        | Expr::Json { span: at, .. }
        | Expr::SchemaOf { span: at, .. }
        | Expr::Int { span: at, .. }
        | Expr::Float { span: at, .. }
        | Expr::Bool { span: at, .. }
        | Expr::List { span: at, .. }
        | Expr::Ref { span: at, .. }
        | Expr::Workflow { span: at }
        | Expr::Variant { span: at, .. }
        | Expr::Record { span: at, .. }
        | Expr::Field { span: at, .. }
        | Expr::Index { span: at, .. }
        | Expr::Accessor { span: at, .. }
        | Expr::Not { span: at, .. }
        | Expr::Binary { span: at, .. }
        | Expr::Predicate { span: at, .. }
        | Expr::CollectionPredicate { span: at, .. } => *at = span,
        Expr::Duration(duration) => duration.span = span,
    }
    expr
}
