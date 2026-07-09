use crate::{CallTarget, HandlerTerminal, RetrySpec, StepDecl, StepOp};

use super::context::Emitter;
use super::error::EmitError;
use super::helpers::{collect_expr_refs, duration_ms, expr, ident};

impl Emitter<'_> {
    /// Emit the call to a bounded `repeat` loop and render the loop function
    /// itself (a top-level tail-recursive function emitted after `execute`).
    pub(super) fn emit_repeat_call(&mut self, step: &StepDecl, cap: &str) -> Result<(), EmitError> {
        let loop_name = format!("{}_loop", ident(&step.name));
        let as_name = step.bind_as.as_ref().map(|bind| bind.name.clone());
        let threads_binding = as_name
            .as_ref()
            .is_some_and(|name| self.bindings.contains_key(name));
        let free = Self::loop_free_names(step, as_name.as_deref());
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

    pub(super) fn render_loop_fn(
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
        if let Some(until) = &step.until {
            self.check_no_opaque_refs(until)?;
        }
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
    pub(super) fn emit_loop_round(
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

    /// Names (beyond the loop counter and the `as` binding) that the loop
    /// body references and therefore must receive as parameters.
    ///
    /// The `as` name is never a free name: the loop body binds it on every
    /// iteration (threaded through parameters when a prior binding exists,
    /// or bound fresh by the `Ok(pattern) ->` arm otherwise), so any
    /// reference to it inside the loop (e.g. from `until`) resolves locally.
    pub(super) fn loop_free_names(step: &StepDecl, as_name: Option<&str>) -> Vec<String> {
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
        refs.retain(|name| as_name != Some(name.as_str()));
        refs
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
pub(super) fn child_retry_params(retry: &RetrySpec) -> (u64, u64, u64, u64) {
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
