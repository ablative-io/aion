//! Read-only semantic queries produced by the AWL checker.
//!
//! This module exposes the types and name resolution the checker already
//! computes. It deliberately does not perform parsing or type inference of its
//! own: [`analyze`] and [`analyze_in`] run the normal checker pipeline and retain
//! its semantic trace alongside diagnostics.

use std::path::Path;

use crate::ast::{Document, ForkHeader, PipeEnd, Statement, Step, TypeBody};
use crate::{CheckError, DocLine, Span};

/// The checker-owned category of a declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    /// The workflow declared by the document.
    Workflow,
    /// A workflow input.
    Input,
    /// A workflow signal.
    Signal,
    /// A workflow outcome.
    Outcome,
    /// A named type.
    Type,
    /// A field of a declared record type.
    Field,
    /// An enum variant.
    Variant,
    /// A worker task queue.
    Worker,
    /// A worker action.
    Action,
    /// A child workflow.
    Child,
    /// An action or child parameter.
    Parameter,
    /// A top-level step or nested substep.
    Step,
    /// A value binding introduced by a statement, loop, or fork.
    Binding,
}

impl DeclarationKind {
    /// Returns the stable, human-readable name of this declaration category.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workflow => "workflow",
            Self::Input => "input",
            Self::Signal => "signal",
            Self::Outcome => "outcome",
            Self::Type => "type",
            Self::Field => "field",
            Self::Variant => "variant",
            Self::Worker => "worker",
            Self::Action => "action",
            Self::Child => "child",
            Self::Parameter => "parameter",
            Self::Step => "step",
            Self::Binding => "binding",
        }
    }
}

/// A declaration selected by the checker's name-resolution rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// Source span of the declaration's name.
    pub span: Span,
    /// Declared name.
    pub name: String,
    /// Checker-owned declaration category.
    pub kind: DeclarationKind,
    /// Normalized `///` documentation, when the declaration has any.
    pub documentation: Option<String>,
}

/// Semantic facts attached to one source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticInfo {
    /// Source span to which these facts apply.
    pub span: Span,
    /// Checker-rendered type at the span, when the construct has a value type.
    pub ty: Option<String>,
    /// Declaration selected for a reference, or the declaration represented by
    /// the span itself.
    pub declaration: Option<Declaration>,
}

/// The result of checking a document while retaining semantic query data.
#[derive(Debug, Clone)]
pub struct SemanticAnalysis {
    diagnostics: Vec<CheckError>,
    info: Vec<SemanticInfo>,
}

impl SemanticAnalysis {
    /// Returns checker diagnostics in the same order as [`crate::check`].
    #[must_use]
    pub fn diagnostics(&self) -> &[CheckError] {
        &self.diagnostics
    }

    /// Returns semantic facts for the narrowest span containing `byte_offset`.
    ///
    /// Declaration-name and reference-name spans therefore win over enclosing
    /// expression spans.
    #[must_use]
    pub fn info_at(&self, byte_offset: usize) -> Option<&SemanticInfo> {
        self.info
            .iter()
            .filter(|info| info.span.start <= byte_offset && byte_offset < info.span.end)
            .min_by_key(|info| info.span.end.saturating_sub(info.span.start))
    }

    /// Returns semantic facts attached to exactly `span`.
    #[must_use]
    pub fn info_for_span(&self, span: Span) -> Option<&SemanticInfo> {
        self.info.iter().find(|info| info.span == span)
    }

    /// Iterates over every span-indexed semantic fact produced by the checker.
    pub fn iter(&self) -> impl Iterator<Item = &SemanticInfo> {
        self.info.iter()
    }

    pub(crate) fn from_parts(diagnostics: Vec<CheckError>, builder: Builder) -> Self {
        Self {
            diagnostics,
            info: builder.info,
        }
    }
}

/// Checks `document` and retains the checker's span-indexed semantic facts.
///
/// Schema imports cannot resolve without a document directory; use
/// [`analyze_in`] when that directory is known.
#[must_use]
pub fn analyze(document: &Document) -> SemanticAnalysis {
    crate::checker::analyze(document, None)
}

/// Checks `document`, resolving schema imports relative to `root`, and retains
/// the checker's span-indexed semantic facts.
#[must_use]
pub fn analyze_in(document: &Document, root: &Path) -> SemanticAnalysis {
    crate::checker::analyze(document, Some(root))
}

#[derive(Debug)]
pub(crate) struct Builder {
    info: Vec<SemanticInfo>,
    declarations: Vec<Declaration>,
}

impl Builder {
    pub(crate) fn new(document: &Document) -> Self {
        let mut builder = Self {
            info: Vec::new(),
            declarations: Vec::new(),
        };
        builder.declare(
            document.name_span,
            &document.name,
            DeclarationKind::Workflow,
            &document.narration,
        );
        for input in &document.inputs {
            builder.declare(input.name_span, &input.name, DeclarationKind::Input, &[]);
        }
        for signal in &document.signals {
            builder.declare(signal.name_span, &signal.name, DeclarationKind::Signal, &[]);
        }
        for outcome in &document.outcomes {
            builder.declare(
                outcome.name_span,
                &outcome.name,
                DeclarationKind::Outcome,
                &[],
            );
        }
        for declared in &document.types {
            builder.declare(
                declared.name_span,
                &declared.name,
                DeclarationKind::Type,
                &declared.docs,
            );
            match &declared.body {
                TypeBody::Record { fields } => {
                    for field in fields {
                        builder.declare(
                            field.name_span,
                            &field.name,
                            DeclarationKind::Field,
                            &field.docs,
                        );
                    }
                }
                TypeBody::Enum { variants } => {
                    for variant in variants {
                        builder.declare(variant.span, &variant.name, DeclarationKind::Variant, &[]);
                    }
                }
                TypeBody::SchemaInline { .. } | TypeBody::SchemaImport { .. } => {}
            }
        }
        for worker in &document.workers {
            builder.declare(
                worker.name_span,
                &worker.name,
                DeclarationKind::Worker,
                &worker.docs,
            );
            for action in &worker.actions {
                builder.declare(
                    action.name_span,
                    &action.name,
                    DeclarationKind::Action,
                    &action.docs,
                );
                for parameter in &action.params {
                    builder.declare(
                        parameter.name_span,
                        &parameter.name,
                        DeclarationKind::Parameter,
                        &[],
                    );
                }
            }
        }
        for child in &document.children {
            builder.declare(
                child.name_span,
                &child.name,
                DeclarationKind::Child,
                &child.docs,
            );
            for parameter in &child.params {
                builder.declare(
                    parameter.name_span,
                    &parameter.name,
                    DeclarationKind::Parameter,
                    &[],
                );
            }
        }
        for step in &document.steps {
            builder.steps(step);
        }
        builder
    }

    fn steps(&mut self, step: &Step) {
        self.declare(
            step.name_span,
            &step.name,
            DeclarationKind::Step,
            &step.docs,
        );
        self.statements(&step.body);
        if let Some(on_failure) = &step.on_failure {
            self.statements(&on_failure.body);
        }
    }

    fn statements(&mut self, statements: &[Statement]) {
        for statement in statements {
            match statement {
                Statement::Call(call) => {
                    if let Some(binding) = &call.bind {
                        self.binding_declaration(binding.span, &binding.name);
                    }
                }
                Statement::Spawn(spawn) => {
                    if let Some(binding) = &spawn.bind {
                        self.binding_declaration(binding.span, &binding.name);
                    }
                }
                Statement::Pipe(pipe) => {
                    if let PipeEnd::Bind(binding) = &pipe.end {
                        self.binding_declaration(binding.span, &binding.name);
                    }
                }
                Statement::Wait(wait) => {
                    self.binding_declaration(wait.bind.span, &wait.bind.name);
                }
                Statement::Fork(fork) => {
                    if let ForkHeader::Collection { var, var_span, .. } = &fork.header {
                        self.binding_declaration(*var_span, var);
                    }
                    self.statements(&fork.body);
                    if let Some(binding) = &fork.join.bind {
                        self.binding_declaration(binding.span, &binding.name);
                    }
                }
                Statement::Loop(looped) => {
                    self.binding_declaration(looped.var_span, &looped.var);
                    if let Some(counter) = &looped.counter {
                        self.binding_declaration(counter.span, &counter.name);
                    }
                    self.statements(&looped.body);
                }
                Statement::SubStep(substep) => self.steps(substep),
                Statement::Sleep(_) | Statement::Route(_) => {}
            }
        }
    }

    fn binding_declaration(&mut self, span: Span, name: &str) {
        self.declare(span, name, DeclarationKind::Binding, &[]);
    }

    fn declare(&mut self, span: Span, name: &str, kind: DeclarationKind, docs: &[DocLine]) {
        let declaration = Declaration {
            span,
            name: name.to_owned(),
            kind,
            documentation: doc_text(docs),
        };
        self.declarations.push(declaration.clone());
        self.entry(span).declaration = Some(declaration);
    }

    pub(crate) fn binding(&mut self, span: Span, name: &str, ty: &str) {
        if !self
            .declarations
            .iter()
            .any(|declaration| declaration.span == span)
        {
            self.binding_declaration(span, name);
        }
        self.entry(span).ty = Some(ty.to_owned());
    }

    pub(crate) fn ty(&mut self, span: Span, ty: &str) {
        self.entry(span).ty = Some(ty.to_owned());
    }

    pub(crate) fn reference(&mut self, span: Span, kind: DeclarationKind, name: &str) {
        let mut matches = self
            .declarations
            .iter()
            .filter(|declaration| declaration.kind == kind && declaration.name == name);
        let declaration = matches.next().cloned();
        if matches.next().is_none() {
            self.entry(span).declaration = declaration;
        }
    }

    pub(crate) fn reference_to(&mut self, span: Span, declaration: Option<Span>) {
        let target = declaration.and_then(|target| {
            self.declarations
                .iter()
                .find(|declaration| declaration.span == target)
                .cloned()
        });
        self.entry(span).declaration = target;
    }

    fn entry(&mut self, span: Span) -> &mut SemanticInfo {
        if let Some(index) = self.info.iter().position(|info| info.span == span) {
            return &mut self.info[index];
        }
        self.info.push(SemanticInfo {
            span,
            ty: None,
            declaration: None,
        });
        let index = self.info.len() - 1;
        &mut self.info[index]
    }
}

fn doc_text(lines: &[DocLine]) -> Option<String> {
    let text = lines
        .iter()
        .map(|line| line.text.strip_prefix(' ').unwrap_or(&line.text).trim_end())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned();
    (!text.is_empty()).then_some(text)
}
