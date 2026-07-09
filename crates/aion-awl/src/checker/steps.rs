use crate::{CallTarget, HandlerBlock, HandlerTerminal, Spanned, StepDecl, StepOp};

use super::{context::Ctx, types::Ty};

impl Ctx<'_> {
    pub(super) fn check_step(&mut self, step: &StepDecl, output: &Ty) {
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
            StepOp::Wait { span, signal } => {
                if let Some(ty) = self.signals.get(signal.as_str()) {
                    ty.clone()
                } else {
                    self.error(*span, format!("unknown signal `{signal}`"));
                    Ty::Unknown
                }
            }
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
}
