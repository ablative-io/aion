use std::fmt::Write as _;

use crate::{CallTarget, HandlerTerminal, StepDecl, StepOp};

use super::context::{Binding, Emitter, TerminatingHandler};
use super::error::EmitError;
use super::helpers::{duration_expr, expr, ident, wrap_doc};

impl Emitter<'_> {
    pub(super) fn execute(&mut self) -> Result<(), EmitError> {
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
    pub(super) fn emit_steps(&mut self, steps: &[StepDecl]) -> Result<(), EmitError> {
        let Some((step, rest)) = steps.split_first() else {
            self.check_no_opaque_refs(&self.document.finish)?;
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
    pub(super) fn check_step(&self, step: &StepDecl) -> Result<(), EmitError> {
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
    pub(super) fn emit_terminating_step(
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
            self.check_no_opaque_refs(when)?;
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

    pub(super) fn emit_terminating_case(
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
    pub(super) fn emit_flat_step(&mut self, step: &StepDecl) -> Result<(), EmitError> {
        if let Some(when) = &step.when {
            self.check_no_opaque_refs(when)?;
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

    pub(super) fn emit_step_expr(&mut self, step: &StepDecl) -> Result<(), EmitError> {
        if let Some(repeat) = &step.repeat {
            self.check_no_opaque_refs(repeat)?;
            let cap = expr(repeat);
            return self.emit_repeat_call(step, &cap);
        }
        self.emit_attempt(step)
    }

    /// Emit one attempt of the step body: the fan-out, timeout, and
    /// non-terminating handler forms around the inner pipeline.
    pub(super) fn emit_attempt(&mut self, step: &StepDecl) -> Result<(), EmitError> {
        if let Some(each) = &step.each {
            // `check_step` guarantees the op is an action call here.
            if let StepOp::Do(CallTarget::Action(call)) = &step.op {
                self.check_no_opaque_refs(&each.in_expr)?;
                let items = expr(&each.in_expr);
                let item_name = ident(&each.name);
                self.line(&format!("workflow.map({items}, fn({item_name}) {{"));
                self.indented_try(|this| {
                    let mut activity = String::new();
                    this.write_activity_value(&mut activity, call, step)?;
                    this.line(&activity);
                    Ok(())
                })?;
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
    pub(super) fn step_inner(&mut self, step: &StepDecl) -> Result<String, EmitError> {
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
