use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fmt::Write as _;
use std::mem;

use crate::{
    ActionDecl, BinaryOp, CallExpr, CallTarget, Document, DurationLiteral, DurationUnit, Expr,
    HandlerBlock, HandlerTerminal, RetrySpec, Span, Spanned, StepDecl, StepOp, TypeRef,
};

/// An error produced while lowering a parsed AWL document to Gleam.
///
/// Emission fails when a document uses a construct the emitter cannot lower
/// faithfully (rather than emitting panicking or non-compiling Gleam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    /// The span of the construct that could not be lowered.
    pub span: Span,
    /// What was wrong and, where possible, what to do instead.
    pub message: String,
}

impl EmitError {
    fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at line {}, column {}",
            self.message, self.span.line, self.span.column
        )
    }
}

impl Error for EmitError {}

/// Emit a complete Gleam workflow module for a parsed AWL document.
///
/// # Errors
///
/// Returns [`EmitError`] when the document uses a construct that cannot be
/// lowered faithfully (for example `each` on a non-action step, a
/// `when`-guarded rebind of a name with no prior binding, or routing fields
/// on a child workflow call).
pub fn emit(document: &Document) -> Result<String, EmitError> {
    Emitter::new(document).emit()
}

/// The type the emitter knows for a value binding while walking the steps.
#[derive(Debug, Clone)]
enum Binding {
    /// The binding has a statically-known AWL type.
    Typed(TypeRef),
    /// The binding is a child-workflow result with no contract in this
    /// revision (the checker's opaque-child rule).
    Opaque,
}

/// A step handler whose terminal is `finish`, which must terminate the whole
/// workflow with that value (continuation nesting).
enum TerminatingHandler<'a> {
    Timeout(&'a HandlerBlock),
    Failure(&'a HandlerBlock),
}

struct Emitter<'a> {
    document: &'a Document,
    out: String,
    indent: usize,
    /// Value bindings in scope, keyed by their original AWL names.
    bindings: HashMap<String, Binding>,
    /// Rendered `repeat` loop functions, emitted after `execute`.
    loop_fns: Vec<String>,
    /// Names of already-rendered loop functions (guarded steps emit their
    /// continuation twice, which would otherwise duplicate the loop).
    loop_fn_names: HashSet<String>,
    /// Emit the encode-only JSON codec used for child workflow inputs.
    uses_child_calls: bool,
    /// Emit the bounded retry-with-backoff helper for child workflow calls.
    uses_child_retry: bool,
}

impl<'a> Emitter<'a> {
    fn new(document: &'a Document) -> Self {
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

    fn emit(mut self) -> Result<String, EmitError> {
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

    fn header(&mut self) {
        if let Some(about) = &self.document.about {
            for line in wrap_doc(&about.text) {
                self.line(&format!("//// {line}"));
            }
        } else {
            self.line("//// Generated from an AWL document.");
        }
        self.blank();
        self.line("import aion/activity");
        self.line("import aion/codec.{type Codec}");
        self.line("import aion/duration");
        self.line("import aion/error");
        self.line("import aion/signal");
        self.line("import aion/workflow");
        self.line("import gleam/dynamic.{type Dynamic}");
        self.line("import gleam/dynamic/decode");
        self.line("import gleam/json");
        self.line("import gleam/list");
        self.line("import gleam/option.{type Option, None, Some}");
        self.blank();
    }

    fn types(&mut self) {
        self.line("pub type AwlError {");
        self.indented(|this| {
            this.line("AwlDecodeInputFailed(String)");
            this.line("AwlActivityFailed(String)");
            this.line("AwlSignalFailed(String)");
            this.line("AwlChildFailed(String)");
            this.line("AwlTimerFailed(String)");
            this.line("AwlTimedOut(String)");
            this.line("AwlFailed");
        });
        self.line("}");
        self.blank();

        for name in self.external_named_types() {
            self.line(&format!("pub type {name} {{"));
            self.indented(|this| {
                let ctor = constructor(&name);
                this.line(&format!("{ctor}(value: String)"));
            });
            self.line("}");
            self.blank();
        }

        self.emit_type(
            &self.input_type_name(),
            self.document
                .inputs
                .iter()
                .map(|field| (field.name.as_str(), &field.ty)),
        );
        for decl in &self.document.types {
            self.emit_type(
                &decl.name,
                decl.fields
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
        }
    }

    fn emit_type<'b, I>(&mut self, name: &str, fields: I)
    where
        I: IntoIterator<Item = (&'b str, &'b TypeRef)>,
    {
        let fields = fields.into_iter().collect::<Vec<_>>();
        self.line(&format!("pub type {name} {{"));
        let ctor = constructor(name);
        self.indented(|this| {
            if fields.is_empty() {
                this.line(&ctor);
            } else {
                this.line(&format!("{ctor}("));
                this.indented(|this| {
                    for (field_name, field_type) in fields {
                        let field_name = ident(field_name);
                        let field_type = gleam_type(field_type);
                        this.line(&format!("{field_name}: {field_type},"));
                    }
                });
                this.line(")");
            }
        });
        self.line("}");
        self.blank();
    }

    fn definition(&mut self) {
        let input_type = self.input_type_name();
        let output_type = self.output_type_name();
        self.line("/// Typed definition binding the codecs to the execute function.");
        self.line(&format!(
            "pub fn definition() -> workflow.WorkflowDefinition({input_type}, {output_type}, AwlError) {{"
        ));
        self.indented(|this| {
            this.line("workflow.define(");
            this.indented(|this| {
                let workflow_name = &this.document.workflow.name;
                let input_codec = snake(&input_type);
                let output_codec = snake(&output_type);
                this.line(&format!("\"{workflow_name}\","));
                this.line(&format!("{input_codec}_codec(),"));
                this.line(&format!("{output_codec}_codec(),"));
                this.line("awl_error_codec(),");
                this.line("execute,");
            });
            this.line(")");
        });
        self.line("}");
        self.blank();
    }

    fn run(&mut self) {
        let input_type = self.input_type_name();
        let output_type = self.output_type_name();
        self.line("/// Engine entry point.");
        self.line("pub fn run(raw_input: Dynamic) -> Result(String, AwlError) {");
        self.indented(|this| {
            this.line("case decode.run(raw_input, decode.string) {");
            this.indented(|this| {
                this.line("Ok(raw_json) ->");
                this.indented(|this| {
                    let input_codec = snake(&input_type);
                    this.line(&format!("case {input_codec}_codec().decode(raw_json) {{"));
                    this.indented(|this| {
                        this.line("Ok(input) ->");
                        this.indented(|this| {
                            this.line("case execute(input) {");
                            this.indented(|this| {
                                let output_codec = snake(&output_type);
                                this.line(&format!(
                                    "Ok(result) -> Ok({output_codec}_codec().encode(result))"
                                ));
                                this.line("Error(workflow_error) -> Error(workflow_error)");
                            });
                            this.line("}");
                        });
                        this.line("Error(codec.DecodeError(reason: reason, path: _)) ->");
                        this.indented(|this| {
                            this.line("Error(AwlDecodeInputFailed(\"failed to decode workflow input: \" <> reason))");
                        });
                    });
                    this.line("}");
                });
                this.line("Error(_) -> Error(AwlDecodeInputFailed(\"workflow input payload was not a string\"))");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();
    }

    fn execute(&mut self) -> Result<(), EmitError> {
        let input_type = self.input_type_name();
        let output_type = self.output_type_name();
        self.line("/// Workflow body generated from the AWL steps.");
        self.line(&format!(
            "pub fn execute(input: {input_type}) -> Result({output_type}, AwlError) {{"
        ));
        let document = self.document;
        self.indented_try(|this| {
            for input in &document.inputs {
                let name = ident(&input.name);
                this.line(&format!("let {name} = input.{name}"));
                this.bindings
                    .insert(input.name.clone(), Binding::Typed(input.ty.clone()));
            }
            if !document.inputs.is_empty() {
                this.blank();
            }
            this.emit_steps(&document.steps)
        })?;
        self.line("}");
        self.blank();
        Ok(())
    }

    /// Emit `steps` followed by the document's `finish`, nesting the
    /// continuation inside the success arm of any step whose handler can
    /// `finish` the workflow early.
    fn emit_steps(&mut self, steps: &[StepDecl]) -> Result<(), EmitError> {
        let Some((step, rest)) = steps.split_first() else {
            let finish = expr(&self.document.finish);
            self.line(&format!("Ok({finish})"));
            return Ok(());
        };
        if let Some(about) = &step.about {
            for line in wrap_doc(&about.text) {
                self.line(&format!("// {line}"));
            }
        }
        self.check_step(step)?;
        if let Some(handler) = terminating_handler(step) {
            self.emit_terminating_step(step, rest, &handler)
        } else {
            self.emit_flat_step(step)?;
            self.blank();
            self.record_binding(step);
            self.emit_steps(rest)
        }
    }

    /// Reject step shapes the emitter cannot lower faithfully.
    fn check_step(&self, step: &StepDecl) -> Result<(), EmitError> {
        if let Some(each) = &step.each {
            if !matches!(step.op, StepOp::Do(CallTarget::Action(_))) {
                return Err(EmitError::new(
                    each.span,
                    format!(
                        "step `{}`: `each` is only supported for action calls",
                        step.name
                    ),
                ));
            }
            if step.repeat.is_some() {
                return Err(EmitError::new(
                    each.span,
                    format!(
                        "step `{}`: `each` and `repeat` cannot be combined on one step",
                        step.name
                    ),
                ));
            }
            if terminating_handler(step).is_some() {
                return Err(EmitError::new(
                    each.span,
                    format!(
                        "step `{}`: a `finish` handler on an `each` step is not supported in this revision",
                        step.name
                    ),
                ));
            }
        }
        if step.until.is_some() && step.repeat.is_none() {
            return Err(EmitError::new(
                step.span,
                format!("step `{}`: `until` requires `repeat up to`", step.name),
            ));
        }
        if step.repeat.is_some() && terminating_handler(step).is_some() {
            return Err(EmitError::new(
                step.span,
                format!(
                    "step `{}`: a `finish` handler on a repeated step is not supported in this revision",
                    step.name
                ),
            ));
        }
        if step.on_timeout.is_some() && step.timeout.is_none() {
            return Err(EmitError::new(
                step.span,
                format!(
                    "step `{}`: `on timeout` requires a `timeout` field",
                    step.name
                ),
            ));
        }
        if step.when.is_some() {
            if let Some(bind) = &step.bind_as {
                if !self.bindings.contains_key(&bind.name) {
                    return Err(EmitError::new(
                        bind.span,
                        format!(
                            "step `{}`: `when`-guarded step rebinds `{}`, but no prior binding of that name exists to flow through when the guard is false",
                            step.name, bind.name
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Emit a step whose handler terminates the workflow with `finish`: the
    /// remainder of the workflow is nested inside the success arm, and the
    /// handler arm returns the workflow output directly.
    fn emit_terminating_step(
        &mut self,
        step: &StepDecl,
        rest: &[StepDecl],
        handler: &TerminatingHandler<'_>,
    ) -> Result<(), EmitError> {
        let inner = self.step_inner(step)?;
        let scrutinee = match handler {
            TerminatingHandler::Timeout(_) => {
                let duration = step.timeout.as_ref().map(duration_expr).unwrap_or_default();
                format!("workflow.with_timeout(fn() {{ {inner} }}, {duration})")
            }
            TerminatingHandler::Failure(_) => inner,
        };
        self.record_binding(step);
        if let Some(when) = &step.when {
            let guard = expr(when);
            self.line(&format!("case {guard} {{"));
            self.indented_try(|this| {
                this.line("True ->");
                this.indented_try(|this| {
                    this.emit_terminating_case(step, rest, &scrutinee, handler)
                })?;
                this.line("False -> {");
                this.indented_try(|this| this.emit_steps(rest))?;
                this.line("}");
                Ok(())
            })?;
            self.line("}");
            Ok(())
        } else {
            self.emit_terminating_case(step, rest, &scrutinee, handler)
        }
    }

    fn emit_terminating_case(
        &mut self,
        step: &StepDecl,
        rest: &[StepDecl],
        scrutinee: &str,
        handler: &TerminatingHandler<'_>,
    ) -> Result<(), EmitError> {
        let pattern = step
            .bind_as
            .as_ref()
            .map_or_else(|| "_".to_owned(), |bind| ident(&bind.name));
        self.line(&format!("case {scrutinee} {{"));
        self.indented_try(|this| {
            this.line(&format!("Ok({pattern}) -> {{"));
            this.indented_try(|this| this.emit_steps(rest))?;
            this.line("}");
            match handler {
                TerminatingHandler::Timeout(block) => {
                    this.emit_handler_arm("Error(error.TimedOutError(_)) ->", block)?;
                    this.line("Error(error.InnerError(inner)) -> Error(inner)");
                    this.line(
                        "Error(error.TimeoutEngineFailure(message)) -> Error(AwlTimerFailed(message))",
                    );
                }
                TerminatingHandler::Failure(block) => {
                    this.emit_handler_arm("Error(_) ->", block)?;
                }
            }
            Ok(())
        })?;
        self.line("}");
        Ok(())
    }

    /// Emit a step in the flat let-chain form (no workflow-terminating
    /// handler on this step).
    fn emit_flat_step(&mut self, step: &StepDecl) -> Result<(), EmitError> {
        if let Some(when) = &step.when {
            let guard = expr(when);
            if let Some(name) = step.bind_as.as_ref().map(|bind| ident(&bind.name)) {
                self.line(&format!("let {name} = case {guard} {{"));
                self.indented_try(|this| {
                    this.line("True -> {");
                    this.indented_try(|this| {
                        this.line(&format!("let assert Ok({name}) ="));
                        this.indented_try(|this| this.emit_step_expr(step))?;
                        this.line(&name);
                        Ok(())
                    })?;
                    this.line("}");
                    this.line(&format!("False -> {name}"));
                    Ok(())
                })?;
                self.line("}");
            } else {
                self.line(&format!("case {guard} {{"));
                self.indented_try(|this| {
                    this.line("True -> {");
                    this.indented_try(|this| {
                        this.line("let assert Ok(_) =");
                        this.indented_try(|this| this.emit_step_expr(step))?;
                        this.line("Nil");
                        Ok(())
                    })?;
                    this.line("}");
                    this.line("False -> Nil");
                    Ok(())
                })?;
                self.line("}");
            }
        } else if let Some(name) = step.bind_as.as_ref().map(|bind| ident(&bind.name)) {
            self.line(&format!("let assert Ok({name}) ="));
            self.indented_try(|this| this.emit_step_expr(step))?;
        } else {
            self.line("let assert Ok(_) =");
            self.indented_try(|this| this.emit_step_expr(step))?;
        }
        Ok(())
    }

    fn emit_step_expr(&mut self, step: &StepDecl) -> Result<(), EmitError> {
        if let Some(repeat) = &step.repeat {
            let cap = expr(repeat);
            return self.emit_repeat_call(step, &cap);
        }
        self.emit_attempt(step)
    }

    /// Emit one attempt of the step body: the fan-out, timeout, and
    /// non-terminating handler forms around the inner pipeline.
    fn emit_attempt(&mut self, step: &StepDecl) -> Result<(), EmitError> {
        if let Some(each) = &step.each {
            // `check_step` guarantees the op is an action call here.
            if let StepOp::Do(CallTarget::Action(call)) = &step.op {
                let items = expr(&each.in_expr);
                let item_name = ident(&each.name);
                self.line(&format!("workflow.map({items}, fn({item_name}) {{"));
                self.indented(|this| {
                    let mut activity = String::new();
                    this.write_activity_value(&mut activity, call, step);
                    this.line(&activity);
                });
                self.line("}) |> map_activity_error");
            }
            return Ok(());
        }

        let inner = self.step_inner(step)?;
        if let Some(timeout) = &step.timeout {
            let duration = duration_expr(timeout);
            self.line(&format!(
                "case workflow.with_timeout(fn() {{ {inner} }}, {duration}) {{"
            ));
            self.indented_try(|this| {
                this.line("Ok(value) -> Ok(value)");
                if let Some(handler) = &step.on_timeout {
                    this.emit_handler_arm("Error(error.TimedOutError(_)) ->", handler)?;
                } else {
                    this.line(
                        "Error(error.TimedOutError(_)) -> Error(AwlTimedOut(\"step timed out\"))",
                    );
                }
                this.line("Error(error.InnerError(inner)) -> Error(inner)");
                this.line(
                    "Error(error.TimeoutEngineFailure(message)) -> Error(AwlTimerFailed(message))",
                );
                Ok(())
            })?;
            self.line("}");
        } else if let Some(handler) = &step.on_failure {
            self.line(&format!("case {inner} {{"));
            self.indented_try(|this| {
                this.line("Ok(value) -> Ok(value)");
                this.emit_handler_arm("Error(_) ->", handler)
            })?;
            self.line("}");
        } else {
            self.line(&inner);
        }
        Ok(())
    }

    /// Build the single-expression pipeline for the step's operation.
    fn step_inner(&mut self, step: &StepDecl) -> Result<String, EmitError> {
        let mut inner = String::new();
        match &step.op {
            StepOp::Do(target) => self.write_call_pipeline(&mut inner, target, step)?,
            StepOp::Wait { signal, .. } => {
                let _ = write!(
                    inner,
                    "workflow.receive({signal}_signal()) |> map_receive_error"
                );
            }
            StepOp::Sleep(duration) => {
                let duration = duration_expr(duration);
                let _ = write!(inner, "workflow.sleep({duration}) |> map_timer_error");
            }
        }
        Ok(inner)
    }

    /// Emit the call to a bounded `repeat` loop and render the loop function
    /// itself (a top-level tail-recursive function emitted after `execute`).
    fn emit_repeat_call(&mut self, step: &StepDecl, cap: &str) -> Result<(), EmitError> {
        let loop_name = format!("{}_loop", ident(&step.name));
        let as_name = step.bind_as.as_ref().map(|bind| bind.name.clone());
        let threads_binding = as_name
            .as_ref()
            .is_some_and(|name| self.bindings.contains_key(name));
        let free = Self::loop_free_names(step, as_name.as_deref(), threads_binding);
        let counter = loop_counter_name(as_name.as_deref(), &free);

        let mut call_args = vec![cap.to_owned()];
        let mut params = vec![counter.clone()];
        if threads_binding {
            if let Some(name) = &as_name {
                call_args.push(ident(name));
                params.push(ident(name));
            }
        }
        for name in &free {
            call_args.push(ident(name));
            params.push(ident(name));
        }
        self.line(&format!("{loop_name}({})", call_args.join(", ")));

        if self.loop_fn_names.insert(loop_name.clone()) {
            let rendered = self.render_loop_fn(step, &loop_name, &params, &counter)?;
            self.loop_fns.push(rendered);
        }
        Ok(())
    }

    fn render_loop_fn(
        &mut self,
        step: &StepDecl,
        loop_name: &str,
        params: &[String],
        counter: &str,
    ) -> Result<String, EmitError> {
        let as_name = step.bind_as.as_ref().map(|bind| ident(&bind.name));
        let threads_binding = step
            .bind_as
            .as_ref()
            .is_some_and(|bind| self.bindings.contains_key(&bind.name));
        let recurse = {
            let mut args = vec![format!("{counter} - 1")];
            args.extend_from_slice(&params[1..]);
            format!("{loop_name}({})", args.join(", "))
        };
        let until = step.until.as_ref().map(expr);
        self.capture(|this| {
            this.line(&format!(
                "/// Bounded `repeat` loop for step `{}`: runs the step body up to the",
                step.name
            ));
            this.line("/// cap, threading the `as` binding through iterations and exiting early");
            this.line("/// when the `until` condition holds.");
            this.line(&format!("fn {loop_name}({}) {{", params.join(", ")));
            this.indented_try(|this| {
                if threads_binding {
                    let bound = as_name.clone().unwrap_or_default();
                    this.line(&format!("case {counter} <= 0 {{"));
                    this.indented_try(|this| {
                        this.line(&format!("True -> Ok({bound})"));
                        this.line("False -> {");
                        this.indented_try(|this| {
                            this.emit_loop_round(step, &bound, until.as_deref(), &recurse)
                        })?;
                        this.line("}");
                        Ok(())
                    })?;
                    this.line("}");
                    Ok(())
                } else {
                    let pattern = as_name.clone().unwrap_or_else(|| "_".to_owned());
                    let exit = as_name.clone().unwrap_or_else(|| "Nil".to_owned());
                    this.line("let attempt =");
                    this.indented_try(|t| t.emit_attempt(step))?;
                    this.line("case attempt {");
                    this.indented_try(|this| {
                        this.line(&format!("Ok({pattern}) ->"));
                        this.indented_try(|this| {
                            let cap_case = format!("case {counter} <= 1 {{");
                            if let Some(until) = &until {
                                this.line(&format!("case {until} {{"));
                                this.indented_try(|this| {
                                    this.line(&format!("True -> Ok({exit})"));
                                    this.line("False ->");
                                    this.indented(|this| {
                                        this.line(&cap_case);
                                        this.indented(|this| {
                                            this.line(&format!("True -> Ok({exit})"));
                                            this.line(&format!("False -> {recurse}"));
                                        });
                                        this.line("}");
                                    });
                                    Ok(())
                                })?;
                                this.line("}");
                            } else {
                                this.line(&cap_case);
                                this.indented(|this| {
                                    this.line(&format!("True -> Ok({exit})"));
                                    this.line(&format!("False -> {recurse}"));
                                });
                                this.line("}");
                            }
                            Ok(())
                        })?;
                        this.line("Error(awl_error) -> Error(awl_error)");
                        Ok(())
                    })?;
                    this.line("}");
                    Ok(())
                }
            })?;
            this.line("}");
            Ok(())
        })
    }

    /// One round of a binding-threading loop: run the attempt, rebind, check
    /// `until`, and recurse or exit with the current binding.
    fn emit_loop_round(
        &mut self,
        step: &StepDecl,
        bound: &str,
        until: Option<&str>,
        recurse: &str,
    ) -> Result<(), EmitError> {
        self.line("let attempt =");
        self.indented_try(|this| this.emit_attempt(step))?;
        self.line("case attempt {");
        self.indented_try(|this| {
            if let Some(until) = until {
                this.line(&format!("Ok({bound}) ->"));
                this.indented(|this| {
                    this.line(&format!("case {until} {{"));
                    this.indented(|this| {
                        this.line(&format!("True -> Ok({bound})"));
                        this.line(&format!("False -> {recurse}"));
                    });
                    this.line("}");
                });
            } else {
                this.line(&format!("Ok({bound}) -> {recurse}"));
            }
            this.line("Error(awl_error) -> Error(awl_error)");
            Ok(())
        })?;
        self.line("}");
        Ok(())
    }

    /// Names (beyond the loop counter and the threaded binding) that the loop
    /// body references and therefore must receive as parameters.
    fn loop_free_names(
        step: &StepDecl,
        as_name: Option<&str>,
        threads_binding: bool,
    ) -> Vec<String> {
        let mut refs = Vec::new();
        match &step.op {
            StepOp::Do(CallTarget::Action(call)) => {
                for arg in &call.args {
                    collect_expr_refs(arg, &mut refs);
                }
            }
            StepOp::Do(CallTarget::Child { args, .. }) => {
                for arg in args {
                    collect_expr_refs(arg, &mut refs);
                }
            }
            StepOp::Wait { .. } | StepOp::Sleep(_) => {}
        }
        if let Some(until) = &step.until {
            collect_expr_refs(until, &mut refs);
        }
        for handler in [&step.on_timeout, &step.on_failure].into_iter().flatten() {
            for action in &handler.actions {
                let args = match action {
                    CallTarget::Action(call) => &call.args,
                    CallTarget::Child { args, .. } => args,
                };
                for arg in args {
                    collect_expr_refs(arg, &mut refs);
                }
            }
            if let HandlerTerminal::Finish(value) = &handler.terminal {
                collect_expr_refs(value, &mut refs);
            }
        }
        refs.retain(|name| {
            let is_threaded = threads_binding && as_name == Some(name.as_str());
            !is_threaded
        });
        refs
    }

    fn emit_handler_arm(&mut self, prefix: &str, handler: &HandlerBlock) -> Result<(), EmitError> {
        let terminal = match &handler.terminal {
            HandlerTerminal::Finish(value) => format!("Ok({})", expr(value)),
            HandlerTerminal::Fail(_) => "Error(AwlFailed)".to_owned(),
        };
        if handler.actions.is_empty() {
            self.line(&format!("{prefix} {terminal}"));
            return Ok(());
        }
        self.line(&format!("{prefix} {{"));
        self.indented_try(|this| {
            for target in &handler.actions {
                let mut inner = String::new();
                this.write_call_pipeline(&mut inner, target, &empty_step())?;
                this.line(&format!("let assert Ok(_) = {inner}"));
            }
            this.line(&terminal);
            Ok(())
        })?;
        self.line("}");
        Ok(())
    }

    /// Record the type the step's `as` binding will have from here on.
    fn record_binding(&mut self, step: &StepDecl) {
        let Some(bind) = &step.bind_as else {
            return;
        };
        let binding = match &step.op {
            StepOp::Do(CallTarget::Action(call)) => {
                self.action(call).map_or(Binding::Opaque, |action| {
                    let returns = action.returns.clone();
                    if step.each.is_some() {
                        Binding::Typed(TypeRef::List {
                            span: step.span,
                            inner: Box::new(returns),
                        })
                    } else {
                        Binding::Typed(returns)
                    }
                })
            }
            // A child result decodes with the rebound name's established
            // codec; a fresh child binding stays opaque (checker rule).
            StepOp::Do(CallTarget::Child { .. }) => self
                .bindings
                .get(&bind.name)
                .cloned()
                .unwrap_or(Binding::Opaque),
            StepOp::Wait { signal, .. } => self
                .document
                .signals
                .iter()
                .find(|decl| decl.name == *signal)
                .map_or(Binding::Opaque, |decl| Binding::Typed(decl.ty.clone())),
            StepOp::Sleep(_) => Binding::Typed(TypeRef::Named {
                span: step.span,
                name: "Nil".to_owned(),
            }),
        };
        self.bindings.insert(bind.name.clone(), binding);
    }

    fn write_activity_value(&self, inner: &mut String, call: &CallExpr, step: &StepDecl) {
        inner.push_str(&call.name);
        inner.push_str("_activity(");
        for (index, arg) in call.args.iter().enumerate() {
            if index > 0 {
                inner.push_str(", ");
            }
            inner.push_str(&expr(arg));
        }
        inner.push(')');
        if let Some(retry) = step
            .retry
            .as_ref()
            .or_else(|| self.action(call).and_then(|a| a.retry.as_ref()))
        {
            inner.push_str(" |> activity.retry(");
            inner.push_str(&retry_policy(retry));
            inner.push(')');
        }
        if let Some(timeout) = step
            .timeout
            .as_ref()
            .or_else(|| self.action(call).and_then(|a| a.timeout.as_ref()))
        {
            inner.push_str(" |> activity.timeout(");
            inner.push_str(&duration_expr(timeout));
            inner.push(')');
        }
        if let Some(queue) = step
            .queue
            .as_ref()
            .or_else(|| self.action(call).and_then(|a| a.queue.as_ref()))
        {
            inner.push_str(" |> activity.task_queue(");
            inner.push_str(&string_lit(queue));
            inner.push(')');
        }
        if let Some(node) = step
            .node
            .as_ref()
            .or_else(|| self.action(call).and_then(|a| a.node.as_ref()))
        {
            inner.push_str(" |> activity.node(");
            inner.push_str(&string_lit(node));
            inner.push(')');
        }
    }

    fn write_call_pipeline(
        &mut self,
        inner: &mut String,
        target: &CallTarget,
        step: &StepDecl,
    ) -> Result<(), EmitError> {
        match target {
            CallTarget::Action(call) => {
                self.write_activity_value(inner, call, step);
                inner.push_str(" |> workflow.run |> map_activity_error");
                Ok(())
            }
            CallTarget::Child {
                span,
                workflow: name,
                args,
            } => self.write_child_pipeline(inner, *span, name, args, step),
        }
    }

    /// Lower a child call to the SDK's string-name spawn: the workflow name
    /// spawns by registration name, the anchor `fn` is a type witness the SDK
    /// never calls, and the input is the named arguments encoded as one JSON
    /// object.
    fn write_child_pipeline(
        &mut self,
        inner: &mut String,
        span: Span,
        name: &str,
        args: &[Expr],
        step: &StepDecl,
    ) -> Result<(), EmitError> {
        if step.queue.is_some() || step.node.is_some() {
            return Err(EmitError::new(
                span,
                format!(
                    "step `{}`: `queue`/`node` routing is not supported on child workflow calls (the aion_flow spawn API has no placement parameters)",
                    step.name
                ),
            ));
        }
        self.uses_child_calls = true;
        let mut fields = Vec::new();
        for arg in args {
            let Expr::Ref {
                name: arg_name,
                span: arg_span,
            } = arg
            else {
                return Err(EmitError::new(
                    arg.span(),
                    "child call arguments must be plain references (bind the value with `as` first) so each argument's name can key the child input record",
                ));
            };
            let ty = match self.bindings.get(arg_name) {
                Some(Binding::Typed(ty)) => ty.clone(),
                Some(Binding::Opaque) => {
                    return Err(EmitError::new(
                        *arg_span,
                        format!(
                            "child argument `{arg_name}` is an opaque child-workflow result and cannot be re-encoded"
                        ),
                    ));
                }
                None => {
                    return Err(EmitError::new(
                        *arg_span,
                        format!(
                            "child argument `{arg_name}` has no binding with a known type in scope"
                        ),
                    ));
                }
            };
            let codec = codec_name(&ty);
            let value = ident(arg_name);
            fields.push(format!("#(\"{arg_name}\", {codec}_to_json({value}))"));
        }
        let input = format!("json.object([{}])", fields.join(", "));
        let output_codec = match step
            .bind_as
            .as_ref()
            .and_then(|bind| self.bindings.get(&bind.name))
        {
            Some(Binding::Typed(ty)) => format!("{}_codec()", codec_name(ty)),
            _ => "nil_codec()".to_owned(),
        };
        let spawn_call = format!(
            "workflow.spawn_and_wait(\"{name}\", fn(_: json.Json) {{ Error(AwlChildFailed(\"child workflow body runs in its own execution\")) }}, {input}, json_value_codec(), {output_codec}, awl_error_codec()) |> map_child_error"
        );
        if let Some(retry) = &step.retry {
            self.uses_child_retry = true;
            let (attempts, delay_ms, multiplier, max_delay_ms) = child_retry_params(retry);
            let _ = write!(
                inner,
                "awl_retry({attempts}, {delay_ms}, {multiplier}, {max_delay_ms}, fn() {{ {spawn_call} }})"
            );
        } else {
            inner.push_str(&spawn_call);
        }
        Ok(())
    }

    fn activity_wrappers(&mut self) {
        for action in &self.document.actions {
            let action_type_name = pascal(&action.name);
            let input_name = format!("{action_type_name}Input");
            self.emit_type(
                &input_name,
                action
                    .params
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
            let action_name = &action.name;
            let return_type = gleam_type(&action.returns);
            if action.params.is_empty() {
                self.line(&format!(
                    "fn {action_name}_activity() -> activity.Activity({input_name}, {return_type}) {{"
                ));
            } else {
                self.line(&format!("fn {action_name}_activity("));
                self.indented(|this| {
                    for param in &action.params {
                        let name = ident(&param.name);
                        let ty = gleam_type(&param.ty);
                        this.line(&format!("{name}: {ty},"));
                    }
                });
                self.line(&format!(
                    ") -> activity.Activity({input_name}, {return_type}) {{"
                ));
            }
            self.indented(|this| {
                this.line("activity.new(");
                this.indented(|this| {
                    let action_name = &action.name;
                    let ctor = constructor(&input_name);
                    this.line(&format!("\"{action_name}\","));
                    if action.params.is_empty() {
                        this.line(&format!("{ctor},"));
                    } else {
                        this.line(&format!("{ctor}("));
                        this.indented(|this| {
                            for param in &action.params {
                                let name = ident(&param.name);
                                this.line(&format!("{name}: {name},"));
                            }
                        });
                        this.line("),");
                    }
                    let input_codec = snake(&input_name);
                    let return_codec = codec_name(&action.returns);
                    this.line(&format!("{input_codec}_codec(),"));
                    this.line(&format!("{return_codec}_codec(),"));
                    this.line("fn(_) { Error(error.terminal(\"activity body is provided by a worker\")) },");
                });
                this.line(")");
            });
            self.line("}");
            self.blank();
        }
    }

    fn signal_refs(&mut self) {
        for signal_decl in &self.document.signals {
            let signal_name = &signal_decl.name;
            let signal_type = gleam_type(&signal_decl.ty);
            self.line(&format!(
                "fn {signal_name}_signal() -> signal.SignalRef({signal_type}) {{"
            ));
            self.indented(|this| {
                let codec = codec_name(&signal_decl.ty);
                this.line(&format!("signal.new(\"{signal_name}\", {codec}_codec())"));
            });
            self.line("}");
            self.blank();
        }
    }

    fn codecs(&mut self) {
        self.line("fn awl_error_codec() -> Codec(AwlError) {");
        self.indented(|this| {
            this.line("codec.json_codec(awl_error_to_json, awl_error_decoder())");
        });
        self.line("}");
        self.blank();
        self.line("fn awl_error_to_json(error_value: AwlError) -> json.Json {");
        self.indented(|this| {
            this.line("case error_value {");
            this.indented(|this| {
                for variant in ["AwlDecodeInputFailed", "AwlActivityFailed", "AwlSignalFailed", "AwlChildFailed", "AwlTimerFailed", "AwlTimedOut"] {
                    this.line(&format!("{variant}(message) -> json.object([#(\"tag\", json.string(\"{variant}\")), #(\"message\", json.string(message))])"));
                }
                this.line("AwlFailed -> json.object([#(\"tag\", json.string(\"AwlFailed\"))])");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();
        self.line("fn awl_error_decoder() -> decode.Decoder(AwlError) {");
        self.indented(|this| {
            this.line("use tag <- decode.field(\"tag\", decode.string)");
            this.line("case tag {");
            this.indented(|this| {
                for variant in [
                    "AwlDecodeInputFailed",
                    "AwlActivityFailed",
                    "AwlSignalFailed",
                    "AwlChildFailed",
                    "AwlTimerFailed",
                    "AwlTimedOut",
                ] {
                    this.line(&format!("\"{variant}\" -> {{"));
                    this.indented(|this| {
                        this.line("use message <- decode.field(\"message\", decode.string)");
                        this.line(&format!("decode.success({variant}(message))"));
                    });
                    this.line("}");
                }
                this.line("\"AwlFailed\" -> decode.success(AwlFailed)");
                this.line("_ -> decode.failure(AwlFailed, \"AwlError\")");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();

        for name in self.external_named_types() {
            self.emit_external_codec(&name);
        }
        self.emit_codec(
            &self.input_type_name(),
            self.document
                .inputs
                .iter()
                .map(|field| (field.name.as_str(), &field.ty)),
        );
        for decl in &self.document.types {
            self.emit_codec(
                &decl.name,
                decl.fields
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
        }
        for action in &self.document.actions {
            let action_type_name = pascal(&action.name);
            self.emit_codec(
                &format!("{action_type_name}Input"),
                action
                    .params
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
        }
        let mut composite_types = Vec::new();
        self.collect_composite_types(&mut composite_types);
        for ty in composite_types {
            self.emit_composite_codec(&ty);
        }
        self.builtin_codecs();
        self.error_mappers();
        self.child_helpers();
    }

    fn emit_external_codec(&mut self, name: &str) {
        let codec_fn = snake(name);
        let constructor_name = constructor(name);
        self.line(&format!("fn {codec_fn}_codec() -> Codec({name}) {{"));
        self.indented(|this| {
            this.line(&format!(
                "codec.json_codec({codec_fn}_to_json, {codec_fn}_decoder())"
            ));
        });
        self.line("}");
        self.line(&format!(
            "fn {codec_fn}_to_json(value: {name}) -> json.Json {{"
        ));
        self.indented(|this| {
            this.line("json.string(value.value)");
        });
        self.line("}");
        self.line(&format!(
            "fn {codec_fn}_decoder() -> decode.Decoder({name}) {{"
        ));
        self.indented(|this| {
            this.line("use value <- decode.then(decode.string)");
            this.line(&format!("decode.success({constructor_name}(value: value))"));
        });
        self.line("}");
        self.blank();
    }

    fn emit_codec<'b, I>(&mut self, name: &str, fields: I)
    where
        I: IntoIterator<Item = (&'b str, &'b TypeRef)>,
    {
        let fields = fields.into_iter().collect::<Vec<_>>();
        let codec_fn = snake(name);
        let value_var = ident(&snake(name));
        self.line(&format!("fn {codec_fn}_codec() -> Codec({name}) {{"));
        self.indented(|this| {
            this.line(&format!(
                "codec.json_codec({codec_fn}_to_json, {codec_fn}_decoder())"
            ));
        });
        self.line("}");
        self.blank();
        if fields.is_empty() {
            self.line(&format!("fn {codec_fn}_to_json(_: {name}) -> json.Json {{"));
        } else {
            self.line(&format!(
                "fn {codec_fn}_to_json({value_var}: {name}) -> json.Json {{"
            ));
        }
        self.indented(|this| {
            this.line("json.object([");
            this.indented(|this| {
                for (field_name, field_type) in &fields {
                    let codec = codec_name(field_type);
                    let access = ident(field_name);
                    this.line(&format!(
                        "#(\"{field_name}\", {codec}_to_json({value_var}.{access})),"
                    ));
                }
            });
            this.line("])");
        });
        self.line("}");
        self.blank();
        self.line(&format!(
            "fn {codec_fn}_decoder() -> decode.Decoder({name}) {{"
        ));
        self.indented(|this| {
            for (field_name, field_type) in &fields {
                let codec = codec_name(field_type);
                let binding = ident(field_name);
                this.line(&format!(
                    "use {binding} <- decode.field(\"{field_name}\", {codec}_decoder())"
                ));
            }
            let ctor = constructor(name);
            if fields.is_empty() {
                this.line(&format!("decode.success({ctor})"));
            } else {
                this.line(&format!("decode.success({ctor}("));
                this.indented(|this| {
                    for (field_name, _) in &fields {
                        let binding = ident(field_name);
                        this.line(&format!("{binding}: {binding},"));
                    }
                });
                this.line("))");
            }
        });
        self.line("}");
        self.blank();
    }

    fn collect_composite_types(&self, types: &mut Vec<TypeRef>) {
        for input in &self.document.inputs {
            collect_composite_type(&input.ty, types);
        }
        if let Some(output) = &self.document.output {
            collect_composite_type(&output.ty, types);
        }
        for signal_decl in &self.document.signals {
            collect_composite_type(&signal_decl.ty, types);
        }
        for decl in &self.document.types {
            for field in &decl.fields {
                collect_composite_type(&field.ty, types);
            }
        }
        for action in &self.document.actions {
            for param in &action.params {
                collect_composite_type(&param.ty, types);
            }
            collect_composite_type(&action.returns, types);
        }
    }

    fn emit_composite_codec(&mut self, ty: &TypeRef) {
        match ty {
            TypeRef::Named { .. } => {}
            TypeRef::List { inner, .. } => {
                let name = codec_name(ty);
                let inner_name = codec_name(inner);
                self.line(&format!(
                    "fn {name}_codec() -> Codec({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!(
                        "codec.json_codec({name}_to_json, {name}_decoder())"
                    ));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_to_json(values: {}) -> json.Json {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("list_to_json(values, {inner_name}_to_json)"));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_decoder() -> decode.Decoder({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("list_decoder({inner_name}_decoder())"));
                });
                self.line("}");
                self.blank();
            }
            TypeRef::Option { inner, .. } => {
                let name = codec_name(ty);
                let inner_name = codec_name(inner);
                self.line(&format!(
                    "fn {name}_codec() -> Codec({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!(
                        "codec.json_codec({name}_to_json, {name}_decoder())"
                    ));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_to_json(value: {}) -> json.Json {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("option_to_json(value, {inner_name}_to_json)"));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_decoder() -> decode.Decoder({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("option_decoder({inner_name}_decoder())"));
                });
                self.line("}");
                self.blank();
            }
        }
    }

    fn builtin_codecs(&mut self) {
        self.line(
            "fn string_codec() -> Codec(String) { codec.json_codec(json.string, decode.string) }",
        );
        self.line("fn int_codec() -> Codec(Int) { codec.json_codec(json.int, decode.int) }");
        self.line(
            "fn float_codec() -> Codec(Float) { codec.json_codec(json.float, decode.float) }",
        );
        self.line("fn bool_codec() -> Codec(Bool) { codec.json_codec(json.bool, decode.bool) }");
        self.line("fn nil_codec() -> Codec(Nil) { codec.json_codec(fn(_) { json.object([]) }, decode.success(Nil)) }");
        self.blank();
        self.line("fn string_to_json(value: String) -> json.Json { json.string(value) }");
        self.line("fn int_to_json(value: Int) -> json.Json { json.int(value) }");
        self.line("fn float_to_json(value: Float) -> json.Json { json.float(value) }");
        self.line("fn bool_to_json(value: Bool) -> json.Json { json.bool(value) }");
        self.line("fn nil_to_json(_: Nil) -> json.Json { json.object([]) }");
        self.blank();
        self.line("fn string_decoder() -> decode.Decoder(String) { decode.string }");
        self.line("fn int_decoder() -> decode.Decoder(Int) { decode.int }");
        self.line("fn float_decoder() -> decode.Decoder(Float) { decode.float }");
        self.line("fn bool_decoder() -> decode.Decoder(Bool) { decode.bool }");
        self.line("fn nil_decoder() -> decode.Decoder(Nil) { decode.success(Nil) }");
        self.blank();
        self.line("fn list_to_json(values: List(a), item_to_json: fn(a) -> json.Json) -> json.Json { json.array(values, item_to_json) }");
        self.line("fn list_decoder(item_decoder: decode.Decoder(a)) -> decode.Decoder(List(a)) { decode.list(item_decoder) }");
        self.line("fn option_to_json(value: Option(a), item_to_json: fn(a) -> json.Json) -> json.Json { json.nullable(value, item_to_json) }");
        self.line("fn option_decoder(item_decoder: decode.Decoder(a)) -> decode.Decoder(Option(a)) { decode.optional(item_decoder) }");
        self.blank();
    }

    fn error_mappers(&mut self) {
        self.line("fn try(result: Result(a, AwlError), next: fn(a) -> Result(b, AwlError)) -> Result(b, AwlError) {");
        self.indented(|this| {
            this.line(
                "case result { Ok(value) -> next(value) Error(awl_error) -> Error(awl_error) }",
            );
        });
        self.line("}");
        self.blank();
        self.line("fn map_activity_error(result: Result(a, error.ActivityError)) -> Result(a, AwlError) {");
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlActivityFailed(\"activity failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line(
            "fn map_receive_error(result: Result(a, error.ReceiveError)) -> Result(a, AwlError) {",
        );
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlSignalFailed(\"signal receive failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line("fn map_child_error(result: Result(a, error.ChildError(AwlError))) -> Result(a, AwlError) {");
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlChildFailed(\"child workflow failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line(
            "fn map_timer_error(result: Result(a, error.EngineError)) -> Result(a, AwlError) {",
        );
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlTimerFailed(\"timer failed\")) }");
        });
        self.line("}");
        self.blank();
    }

    fn child_helpers(&mut self) {
        if self.uses_child_calls {
            self.line("/// Encode-only codec for child workflow inputs: the parent assembles the");
            self.line("/// child's input record as JSON and never decodes it back.");
            self.line("fn json_value_codec() -> Codec(json.Json) {");
            self.indented(|this| {
                this.line("codec.Codec(");
                this.indented(|this| {
                    this.line("encode: json.to_string,");
                    this.line("decode: fn(_) {");
                    this.indented(|this| {
                        this.line("Error(codec.DecodeError(reason: \"child call input is encode-only\", path: []))");
                    });
                    this.line("},");
                });
                this.line(")");
            });
            self.line("}");
            self.blank();
        }
        if self.uses_child_retry {
            self.line("/// Bounded retry with backoff for child workflow calls: `attempts` total");
            self.line("/// attempts with an SDK timer sleep between them (fixed when `multiplier`");
            self.line("/// is 1, exponential capped at `max_delay_ms` otherwise).");
            self.line("fn awl_retry(");
            self.indented(|this| {
                this.line("attempts: Int,");
                this.line("delay_ms: Int,");
                this.line("multiplier: Int,");
                this.line("max_delay_ms: Int,");
                this.line("operation: fn() -> Result(value, AwlError),");
            });
            self.line(") -> Result(value, AwlError) {");
            self.indented(|this| {
                this.line("case operation() {");
                this.indented(|this| {
                    this.line("Ok(value) -> Ok(value)");
                    this.line("Error(awl_error) ->");
                    this.indented(|this| {
                        this.line("case attempts <= 1 {");
                        this.indented(|this| {
                            this.line("True -> Error(awl_error)");
                            this.line("False ->");
                            this.indented(|this| {
                                this.line("case workflow.sleep(duration.milliseconds(delay_ms)) {");
                                this.indented(|this| {
                                    this.line("Ok(_) -> {");
                                    this.indented(|this| {
                                        this.line("let next_delay_ms = delay_ms * multiplier");
                                        this.line("let capped_delay_ms = case next_delay_ms > max_delay_ms {");
                                        this.indented(|this| {
                                            this.line("True -> max_delay_ms");
                                            this.line("False -> next_delay_ms");
                                        });
                                        this.line("}");
                                        this.line("awl_retry(attempts - 1, capped_delay_ms, multiplier, max_delay_ms, operation)");
                                    });
                                    this.line("}");
                                    this.line("Error(_) -> Error(AwlTimerFailed(\"timer failed while backing off\"))");
                                });
                                this.line("}");
                            });
                        });
                        this.line("}");
                    });
                });
                this.line("}");
            });
            self.line("}");
            self.blank();
        }
    }

    fn external_named_types(&self) -> Vec<String> {
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

    fn collect_named_refs(&self, names: &mut Vec<String>) {
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

    fn action(&self, call: &CallExpr) -> Option<&ActionDecl> {
        self.document
            .actions
            .iter()
            .find(|action| action.name == call.name)
    }

    fn input_type_name(&self) -> String {
        let workflow_type_name = pascal(&self.document.workflow.name);
        format!("{workflow_type_name}Input")
    }

    fn output_type_name(&self) -> String {
        self.document
            .output
            .as_ref()
            .map_or_else(|| "Nil".to_owned(), |decl| gleam_type(&decl.ty))
    }

    fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }

    fn indented(&mut self, f: impl FnOnce(&mut Self)) {
        self.indent += 1;
        f(self);
        self.indent -= 1;
    }

    fn indented_try(
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
    fn capture(
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

/// Which handler on this step, if any, terminates the workflow with `finish`.
///
/// A `timeout` field takes over the step's outcome handling, so `on failure`
/// is only consulted when no timeout is present (matching the attempt
/// lowering).
fn terminating_handler(step: &StepDecl) -> Option<TerminatingHandler<'_>> {
    if step.timeout.is_some() {
        let handler = step.on_timeout.as_ref()?;
        matches!(handler.terminal, HandlerTerminal::Finish(_))
            .then_some(TerminatingHandler::Timeout(handler))
    } else {
        let handler = step.on_failure.as_ref()?;
        matches!(handler.terminal, HandlerTerminal::Finish(_))
            .then_some(TerminatingHandler::Failure(handler))
    }
}

/// Pick a loop-counter name that cannot shadow a name the loop body needs.
fn loop_counter_name(as_name: Option<&str>, free: &[String]) -> String {
    let mut counter = "remaining".to_owned();
    let taken =
        |candidate: &str| as_name == Some(candidate) || free.iter().any(|name| name == candidate);
    while taken(&counter) {
        counter.push('_');
    }
    counter
}

/// `retry` parameters for the generated `awl_retry` child helper:
/// `(attempts, initial delay ms, multiplier, max delay ms)`.
fn child_retry_params(retry: &RetrySpec) -> (u64, u64, u64, u64) {
    match retry {
        RetrySpec::Every { count, every, .. } => {
            let delay = duration_ms(every);
            (*count, delay, 1, delay)
        }
        RetrySpec::Backoff {
            count, min, max, ..
        } => (*count, duration_ms(min), 2, duration_ms(max)),
    }
}

fn empty_step() -> StepDecl {
    StepDecl {
        span: crate::Span {
            start: 0,
            end: 0,
            line: 0,
            column: 0,
        },
        trivia: crate::Trivia::default(),
        name: String::new(),
        about: None,
        when: None,
        each: None,
        op: StepOp::Sleep(DurationLiteral {
            span: crate::Span {
                start: 0,
                end: 0,
                line: 0,
                column: 0,
            },
            magnitude: 0,
            unit: DurationUnit::Seconds,
        }),
        repeat: None,
        until: None,
        retry: None,
        timeout: None,
        on_timeout: None,
        on_failure: None,
        bind_as: None,
        queue: None,
        node: None,
        leading_comments: Vec::new(),
        trailing_comments: Vec::new(),
    }
}

fn collect_named_ref(ty: &TypeRef, names: &mut Vec<String>) {
    match ty {
        TypeRef::Named { name, .. } => {
            if !names.iter().any(|seen| seen == name) {
                names.push(name.clone());
            }
        }
        TypeRef::List { inner, .. } | TypeRef::Option { inner, .. } => {
            collect_named_ref(inner, names);
        }
    }
}

/// Collect the reference names an expression mentions, in first-use order.
fn collect_expr_refs(value: &Expr, names: &mut Vec<String>) {
    match value {
        Expr::String { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_) => {}
        Expr::List { items, .. } => {
            for item in items {
                collect_expr_refs(item, names);
            }
        }
        Expr::Ref { name, .. } => {
            if !names.iter().any(|seen| seen == name) {
                names.push(name.clone());
            }
        }
        Expr::Field { base, .. } => collect_expr_refs(base, names),
        Expr::Record { fields, .. } => {
            for field in fields {
                collect_expr_refs(&field.value, names);
            }
        }
        Expr::Not { expr: inner, .. } => collect_expr_refs(inner, names),
        Expr::Binary { left, right, .. } => {
            collect_expr_refs(left, names);
            collect_expr_refs(right, names);
        }
    }
}

fn is_builtin_type(name: &str) -> bool {
    matches!(name, "String" | "Int" | "Float" | "Bool" | "Nil")
}

fn collect_composite_type(ty: &TypeRef, types: &mut Vec<TypeRef>) {
    match ty {
        TypeRef::Named { .. } => {}
        TypeRef::List { inner, .. } | TypeRef::Option { inner, .. } => {
            collect_composite_type(inner, types);
            if !types.iter().any(|seen| codec_name(seen) == codec_name(ty)) {
                types.push(ty.clone());
            }
        }
    }
}

fn gleam_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => name.clone(),
        TypeRef::List { inner, .. } => {
            let inner = gleam_type(inner);
            format!("List({inner})")
        }
        TypeRef::Option { inner, .. } => {
            let inner = gleam_type(inner);
            format!("Option({inner})")
        }
    }
}

fn codec_name(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => snake(name),
        TypeRef::List { inner, .. } => {
            let inner = codec_name(inner);
            format!("list_{inner}")
        }
        TypeRef::Option { inner, .. } => {
            let inner = codec_name(inner);
            format!("option_{inner}")
        }
    }
}

fn expr(value: &Expr) -> String {
    match value {
        Expr::String { value, .. } => string_lit(value),
        Expr::Int { value, .. } => value.to_string(),
        Expr::Float { value, .. } => value.clone(),
        Expr::Bool { value, .. } => if *value { "True" } else { "False" }.to_owned(),
        Expr::Duration(duration) => duration_expr(duration),
        Expr::List { items, .. } => {
            let values = items.iter().map(expr).collect::<Vec<_>>().join(", ");
            format!("[{values}]")
        }
        Expr::Ref { name, .. } => ident(name),
        Expr::Field { base, field, .. } => {
            let base = expr(base);
            let field = ident(field);
            format!("{base}.{field}")
        }
        Expr::Record { name, fields, .. } => {
            let ctor = constructor(name);
            if fields.is_empty() {
                return ctor;
            }
            let fields = fields
                .iter()
                .map(|field| {
                    let name = ident(&field.name);
                    let value = expr(&field.value);
                    format!("{name}: {value}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{ctor}({fields})")
        }
        Expr::Not { expr: inner, .. } => {
            let inner = parenthesized(inner);
            format!("!{inner}")
        }
        Expr::Binary {
            left, op, right, ..
        } => {
            let left = parenthesized(left);
            let op = binary_op(*op);
            let right = parenthesized(right);
            format!("{left} {op} {right}")
        }
    }
}

fn parenthesized(value: &Expr) -> String {
    match value {
        Expr::String { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::List { .. }
        | Expr::Ref { .. }
        | Expr::Field { .. }
        | Expr::Record { .. } => expr(value),
        Expr::Not { .. } | Expr::Binary { .. } => {
            let value = expr(value);
            format!("({value})")
        }
    }
}

fn binary_op(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "||",
        BinaryOp::And => "&&",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Add => "<>",
    }
}

fn duration_ms(duration: &DurationLiteral) -> u64 {
    match duration.unit {
        DurationUnit::Seconds => duration.magnitude.saturating_mul(1_000),
        DurationUnit::Minutes => duration.magnitude.saturating_mul(60_000),
        DurationUnit::Hours => duration.magnitude.saturating_mul(3_600_000),
        DurationUnit::Days => duration.magnitude.saturating_mul(86_400_000),
    }
}

fn duration_expr(duration: &DurationLiteral) -> String {
    let milliseconds = duration_ms(duration);
    format!("duration.milliseconds({milliseconds})")
}

fn retry_policy(retry: &RetrySpec) -> String {
    match retry {
        RetrySpec::Every { count, every, .. } => format!(
            "activity.RetryPolicy(max_attempts: {}, backoff: activity.Fixed({}))",
            count,
            duration_expr(every)
        ),
        RetrySpec::Backoff {
            count, min, max, ..
        } => format!(
            "activity.RetryPolicy(max_attempts: {}, backoff: activity.Exponential(initial: {}, multiplier: 2.0, max: {}))",
            count,
            duration_expr(min),
            duration_expr(max)
        ),
    }
}

fn string_lit(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn constructor(name: &str) -> String {
    pascal(name)
}

fn pascal(name: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for character in name.chars() {
        if character == '_' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    out
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// Reserved words in Gleam that cannot be used as value identifiers.
fn is_gleam_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "assert"
            | "auto"
            | "case"
            | "const"
            | "delegate"
            | "derive"
            | "echo"
            | "else"
            | "fn"
            | "if"
            | "implement"
            | "import"
            | "let"
            | "macro"
            | "opaque"
            | "panic"
            | "pub"
            | "test"
            | "todo"
            | "type"
            | "use"
    )
}

/// Sanitize an AWL identifier for emission: Gleam reserved words gain a
/// trailing underscore, applied consistently at every emission site.
fn ident(name: &str) -> String {
    if is_gleam_keyword(name) {
        format!("{name}_")
    } else {
        name.to_owned()
    }
}

fn wrap_doc(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    vec![text.to_owned()]
}
