//! The emitter context: declaration indexes, generated-name assignments, the
//! output buffer with indentation helpers, and the feature flags that decide
//! which imports and helpers the assembled module needs.

use std::collections::BTreeMap;
use std::mem;

use crate::ast::{ActionDecl, ChildDecl, Document, RouteDirection, SignalDecl};

use super::error::EmitError;
use super::names::pascal;
use super::types::{GType, TypeEnv};

/// Everything a lowering pass needs to know about one workflow outcome.
#[derive(Debug, Clone)]
pub(crate) struct OutcomeInfo {
    /// Payload type.
    pub(crate) ty: GType,
    /// Engine terminal direction.
    pub(crate) direction: RouteDirection,
    /// Union constructor name (success outcomes only).
    pub(crate) constructor: Option<String>,
}

/// Feature flags the lowering passes set; the assembled header and helper
/// sections read them afterwards.
#[derive(Debug, Default, Clone)]
pub(crate) struct Flags {
    pub(crate) uses_list_module: bool,
    pub(crate) uses_child_module: bool,
    /// `gleam/<module>` comparator imports a `sort` stage needs.
    pub(crate) compare_modules: std::collections::BTreeSet<&'static str>,
    /// Actions dispatched from a heterogeneous parallel group: each needs a
    /// raw (`Activity(String, String)`) wrapper twin so differently-typed
    /// branches can share one `workflow.all` list.
    pub(crate) raw_actions: std::collections::BTreeSet<String>,
}

pub(crate) struct Emitter<'a> {
    pub(crate) document: &'a Document,
    pub(crate) env: TypeEnv,
    /// Action name → (worker/task-queue name, declaration).
    pub(crate) actions: BTreeMap<&'a str, (&'a str, &'a ActionDecl)>,
    pub(crate) children: BTreeMap<&'a str, &'a ChildDecl>,
    pub(crate) signals: BTreeMap<&'a str, &'a SignalDecl>,
    pub(crate) outcomes: BTreeMap<&'a str, OutcomeInfo>,
    /// Global binding name → type (single-assignment surface).
    pub(crate) bindings: BTreeMap<String, GType>,
    /// Generated input record name (`<Workflow>Input`).
    pub(crate) input_type: String,
    /// Generated outcome union name, `None` when no success outcome exists
    /// (the output type is then `Nil`).
    pub(crate) union_type: Option<String>,
    /// Action name → generated `<Action>Input` record name.
    pub(crate) action_inputs: BTreeMap<String, String>,
    pub(crate) flags: Flags,
    /// Rendered loop functions, appended after the step functions.
    pub(crate) loop_fns: Vec<String>,
    /// Monotonic counter for generated loop-function names.
    pub(crate) loop_counter: usize,
    /// Monotonic counter for nested collection-predicate item names.
    pub(crate) predicate_counter: usize,
    pub(crate) out: String,
    pub(crate) indent: usize,
}

impl<'a> Emitter<'a> {
    pub(crate) fn new(document: &'a Document, env: TypeEnv) -> Result<Self, EmitError> {
        let mut env = env;
        let mut actions: BTreeMap<&str, (&str, &ActionDecl)> = BTreeMap::new();
        for worker in &document.workers {
            for action in &worker.actions {
                if actions
                    .insert(action.name.as_str(), (worker.name.as_str(), action))
                    .is_some()
                {
                    return Err(EmitError::new(
                        action.name_span,
                        format!(
                            "action `{}` is declared on more than one worker — generated \
                             wrapper names collide",
                            action.name
                        ),
                    ));
                }
            }
        }
        let mut children = BTreeMap::new();
        for child in &document.children {
            if children.insert(child.name.as_str(), child).is_some() {
                return Err(EmitError::new(
                    child.name_span,
                    format!("child `{}` is declared more than once", child.name),
                ));
            }
        }
        let mut signals = BTreeMap::new();
        for signal in &document.signals {
            if signals.insert(signal.name.as_str(), signal).is_some() {
                return Err(EmitError::new(
                    signal.name_span,
                    format!("signal `{}` is declared more than once", signal.name),
                ));
            }
        }

        let workflow_pascal = pascal(&document.name);
        let input_type = env.names.fresh(&format!("{workflow_pascal}Input"));
        let has_success = document
            .outcomes
            .iter()
            .any(|outcome| matches!(outcome.direction, RouteDirection::Success));
        let union_type = has_success.then(|| env.names.fresh(&format!("{workflow_pascal}Outcome")));

        let mut outcomes = BTreeMap::new();
        for outcome in &document.outcomes {
            let constructor = matches!(outcome.direction, RouteDirection::Success).then(|| {
                env.names
                    .fresh(&format!("{}Outcome", pascal(&outcome.name)))
            });
            let info = OutcomeInfo {
                ty: super::types::type_ref_to_g(&outcome.ty),
                direction: outcome.direction,
                constructor,
            };
            if outcomes.insert(outcome.name.as_str(), info).is_some() {
                return Err(EmitError::new(
                    outcome.name_span,
                    format!("outcome `{}` is declared more than once", outcome.name),
                ));
            }
        }

        let mut action_inputs = BTreeMap::new();
        for (name, _) in actions.values().map(|(queue, action)| (action, queue)) {
            let record = env.names.fresh(&format!("{}Input", pascal(&name.name)));
            action_inputs.insert(name.name.clone(), record);
        }

        Ok(Self {
            document,
            env,
            actions,
            children,
            signals,
            outcomes,
            bindings: BTreeMap::new(),
            input_type,
            union_type,
            action_inputs,
            flags: Flags::default(),
            loop_fns: Vec::new(),
            loop_counter: 0,
            predicate_counter: 0,
            out: String::new(),
            indent: 0,
        })
    }

    /// The Gleam output type of `execute` (the outcome union, or `Nil` when
    /// the workflow declares no success outcome).
    pub(crate) fn output_type(&self) -> String {
        self.union_type.clone().unwrap_or_else(|| "Nil".to_owned())
    }

    /// A fully-qualified reference to a wire type's `_to_json` function: a
    /// builtin leaf resolves to the SDK's `awlc.<leaf>_to_json`, a named or
    /// composite type to the module-generated `<stem>_to_json`.
    pub(crate) fn to_json_fn(&self, ty: &GType) -> String {
        self.codec_ref(ty, "to_json")
    }

    /// A fully-qualified reference to a wire type's `_decoder` function.
    pub(crate) fn decoder_fn(&self, ty: &GType) -> String {
        self.codec_ref(ty, "decoder")
    }

    /// A fully-qualified reference to a wire type's `_codec` function.
    pub(crate) fn codec_fn(&self, ty: &GType) -> String {
        self.codec_ref(ty, "codec")
    }

    fn codec_ref(&self, ty: &GType, suffix: &str) -> String {
        let stem = self.env.codec_name(ty);
        if self.env.is_leaf(ty) {
            format!("awlc.{stem}_{suffix}")
        } else {
            format!("{stem}_{suffix}")
        }
    }

    pub(crate) fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    pub(crate) fn blank(&mut self) {
        self.out.push('\n');
    }

    pub(crate) fn indented(&mut self, f: impl FnOnce(&mut Self)) {
        self.indent += 1;
        f(self);
        self.indent -= 1;
    }

    pub(crate) fn indented_try(
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
    pub(crate) fn capture(
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
