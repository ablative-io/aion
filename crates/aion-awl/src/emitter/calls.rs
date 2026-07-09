use std::fmt::Write as _;

use crate::{
    CallExpr, CallTarget, DurationLiteral, DurationUnit, Expr, HandlerBlock, HandlerTerminal, Span,
    Spanned, StepDecl, StepOp, TypeRef,
};

use super::context::{Binding, Emitter};
use super::error::{EmitError, opaque_ref_error};
use super::helpers::{codec_name, duration_expr, expr, ident, retry_policy, string_lit};
use super::loops::child_retry_params;

impl Emitter<'_> {
    /// Reject an expression that reads an opaque (untyped) child-workflow
    /// binding anywhere but the two contexts that are allowed to carry one
    /// unrendered (the `as` clause that names it, and threading it as a
    /// plain child-call argument, which `write_child_pipeline` re-checks
    /// itself since it never calls this helper).
    ///
    /// The checker's opaque-child rule leaves these bindings with no known
    /// Gleam type, so splicing one into any other generated expression
    /// (an activity argument, a `when`/`until` guard, a `finish` value, an
    /// `each` source, a record field, ...) would compile to a value whose
    /// real shape (`Nil`) mismatches whatever the site expects. Threading a
    /// *rebound* child result — one whose binding took an established type
    /// from a prior binding of the same name — stays allowed, since by then
    /// `self.bindings` holds `Binding::Typed`, not `Binding::Opaque`.
    pub(super) fn check_no_opaque_refs(&self, value: &Expr) -> Result<(), EmitError> {
        match value {
            Expr::String { .. }
            | Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Bool { .. }
            | Expr::Duration(_) => Ok(()),
            Expr::List { items, .. } => {
                for item in items {
                    self.check_no_opaque_refs(item)?;
                }
                Ok(())
            }
            Expr::Ref { name, span } => {
                if matches!(self.bindings.get(name), Some(Binding::Opaque)) {
                    return Err(opaque_ref_error(name, *span));
                }
                Ok(())
            }
            Expr::Field { base, .. } => self.check_no_opaque_refs(base),
            Expr::Record { fields, .. } => {
                for field in fields {
                    self.check_no_opaque_refs(&field.value)?;
                }
                Ok(())
            }
            Expr::Not { expr: inner, .. } => self.check_no_opaque_refs(inner),
            Expr::Binary { left, right, .. } => {
                self.check_no_opaque_refs(left)?;
                self.check_no_opaque_refs(right)
            }
        }
    }

    pub(super) fn emit_handler_arm(
        &mut self,
        prefix: &str,
        handler: &HandlerBlock,
    ) -> Result<(), EmitError> {
        let terminal = match &handler.terminal {
            HandlerTerminal::Finish(value) => {
                self.check_no_opaque_refs(value)?;
                format!("Ok({})", expr(value))
            }
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
    pub(super) fn record_binding(&mut self, step: &StepDecl) {
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

    pub(super) fn write_activity_value(
        &self,
        inner: &mut String,
        call: &CallExpr,
        step: &StepDecl,
    ) -> Result<(), EmitError> {
        inner.push_str(&call.name);
        inner.push_str("_activity(");
        for (index, arg) in call.args.iter().enumerate() {
            if index > 0 {
                inner.push_str(", ");
            }
            self.check_no_opaque_refs(arg)?;
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
        Ok(())
    }

    pub(super) fn write_call_pipeline(
        &mut self,
        inner: &mut String,
        target: &CallTarget,
        step: &StepDecl,
    ) -> Result<(), EmitError> {
        match target {
            CallTarget::Action(call) => {
                self.write_activity_value(inner, call, step)?;
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
    pub(super) fn write_child_pipeline(
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
                Some(Binding::Opaque) => return Err(opaque_ref_error(arg_name, *arg_span)),
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
