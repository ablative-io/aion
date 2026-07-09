use std::collections::{HashMap, HashSet};
use std::mem;

use crate::{ActionDecl, CallExpr, Document, HandlerBlock, TypeRef};

use super::error::EmitError;
use super::helpers::{collect_named_ref, gleam_type, is_builtin_type, pascal};

/// The type the emitter knows for a value binding while walking the steps.
#[derive(Debug, Clone)]
pub(super) enum Binding {
    /// The binding has a statically-known AWL type.
    Typed(TypeRef),
    /// The binding is a child-workflow result with no contract in this
    /// revision (the checker's opaque-child rule).
    Opaque,
}

/// A step handler whose terminal is `finish`, which must terminate the whole
/// workflow with that value (continuation nesting).
pub(super) enum TerminatingHandler<'a> {
    Timeout(&'a HandlerBlock),
    Failure(&'a HandlerBlock),
}

pub(super) struct Emitter<'a> {
    pub(super) document: &'a Document,
    pub(super) out: String,
    pub(super) indent: usize,
    /// Value bindings in scope, keyed by their original AWL names.
    pub(super) bindings: HashMap<String, Binding>,
    /// Rendered `repeat` loop functions, emitted after `execute`.
    pub(super) loop_fns: Vec<String>,
    /// Names of already-rendered loop functions (guarded steps emit their
    /// continuation twice, which would otherwise duplicate the loop).
    pub(super) loop_fn_names: HashSet<String>,
    /// Emit the encode-only JSON codec used for child workflow inputs.
    pub(super) uses_child_calls: bool,
    /// Emit the bounded retry-with-backoff helper for child workflow calls.
    pub(super) uses_child_retry: bool,
}

impl<'a> Emitter<'a> {
    pub(super) fn new(document: &'a Document) -> Self {
        Self {
            document,
            out: String::new(),
            indent: 0,
            bindings: HashMap::new(),
            loop_fns: Vec::new(),
            loop_fn_names: HashSet::new(),
            uses_child_calls: false,
            uses_child_retry: false,
        }
    }

    pub(super) fn emit(mut self) -> Result<String, EmitError> {
        self.header();
        self.types();
        self.definition();
        self.run();
        self.execute()?;
        let loop_fns = mem::take(&mut self.loop_fns);
        for loop_fn in loop_fns {
            self.out.push_str(&loop_fn);
            self.blank();
        }
        self.activity_wrappers();
        self.signal_refs();
        self.codecs();
        Ok(self.out)
    }

    pub(super) fn external_named_types(&self) -> Vec<String> {
        let declared = self
            .document
            .types
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>();
        let mut names = Vec::new();
        self.collect_named_refs(&mut names);
        names
            .into_iter()
            .filter(|name| !is_builtin_type(name))
            .filter(|name| !declared.iter().any(|declared_name| declared_name == name))
            .collect()
    }

    pub(super) fn collect_named_refs(&self, names: &mut Vec<String>) {
        for input in &self.document.inputs {
            collect_named_ref(&input.ty, names);
        }
        if let Some(output) = &self.document.output {
            collect_named_ref(&output.ty, names);
        }
        for signal_decl in &self.document.signals {
            collect_named_ref(&signal_decl.ty, names);
        }
        for decl in &self.document.types {
            for field in &decl.fields {
                collect_named_ref(&field.ty, names);
            }
        }
        for action in &self.document.actions {
            for param in &action.params {
                collect_named_ref(&param.ty, names);
            }
            collect_named_ref(&action.returns, names);
        }
    }

    pub(super) fn action(&self, call: &CallExpr) -> Option<&ActionDecl> {
        self.document
            .actions
            .iter()
            .find(|action| action.name == call.name)
    }

    pub(super) fn input_type_name(&self) -> String {
        let workflow_type_name = pascal(&self.document.workflow.name);
        format!("{workflow_type_name}Input")
    }

    pub(super) fn output_type_name(&self) -> String {
        self.document
            .output
            .as_ref()
            .map_or_else(|| "Nil".to_owned(), |decl| gleam_type(&decl.ty))
    }

    pub(super) fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    pub(super) fn blank(&mut self) {
        self.out.push('\n');
    }

    pub(super) fn indented(&mut self, f: impl FnOnce(&mut Self)) {
        self.indent += 1;
        f(self);
        self.indent -= 1;
    }

    pub(super) fn indented_try(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<(), EmitError>,
    ) -> Result<(), EmitError> {
        self.indent += 1;
        let result = f(self);
        self.indent -= 1;
        result
    }

    /// Run `f` against a fresh output buffer at indent zero and return the
    /// text it produced, restoring the main buffer afterwards.
    pub(super) fn capture(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<(), EmitError>,
    ) -> Result<String, EmitError> {
        let saved_out = mem::take(&mut self.out);
        let saved_indent = mem::replace(&mut self.indent, 0);
        let result = f(self);
        let captured = mem::replace(&mut self.out, saved_out);
        self.indent = saved_indent;
        result.map(|()| captured)
    }
}
