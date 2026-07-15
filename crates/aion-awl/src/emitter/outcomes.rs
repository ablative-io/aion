//! Route and outcome lowering: workflow-outcome returns (`Ok(Ctor(payload))`
//! on the success path, `Error(AwlOutcomeFailure(…))` on the failure path),
//! nested-flow exit returns (`Ok(payload)` for a subflow outcome, `Ok(<
//! collected binding>)` for a region's close), step routes as region tail
//! calls, sibling-substep and parent-arm resolution, enum-total `case`
//! forms, and `when`-cascades with the one flow-typing rule (`is present`
//! unwraps the guarded binding in its arm).

use std::fmt::Write as _;

use crate::RouteDirection;
use crate::Spanned;
use crate::ast::{
    BinaryOp, Expr, Guard, OutcomeClause, PredicateKind, RoutePayload, RouteTarget, Statement,
};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, expr_type, render_arg_for, render_expr};
use super::names::{ident, string_lit};
use super::pipes::wrap_optional;
use super::steps::{ExitKind, FlowCtx, Frame};
use super::stmts::flush_prelude;
use super::types::GType;

/// Emit the tail expression for a route: a nested-flow exit, a workflow
/// outcome return, a step (region) call, a sibling substep call, or a
/// parent outcome arm fired.
pub(super) fn emit_route(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    frame: Frame<'_>,
    target: &RouteTarget,
    scope: &Scope,
    piped: Option<(String, GType)>,
) -> Result<(), EmitError> {
    // Substep frame: siblings first, then parent outcome arms.
    if let Some((parent_index, split)) = frame.sub {
        let parent = &flow.steps[parent_index];
        for (position, statement) in parent.body[split..].iter().enumerate() {
            if let Statement::SubStep(candidate) = statement
                && candidate.name == target.name
            {
                let args = flow
                    .plan
                    .sub_params(parent_index, position)
                    .iter()
                    .map(|name| ident(name))
                    .collect::<Vec<_>>()
                    .join(", ");
                emitter.line(&format!(
                    "{}({args})",
                    flow.sub_fn(&parent.name, &candidate.name)
                ));
                return Ok(());
            }
        }
        if let Some(clause) = parent
            .outcomes
            .iter()
            .find(|clause| clause.name == target.name)
        {
            // Fire the parent arm: evaluate ITS route in the parent frame.
            let parent_frame = Frame {
                step_name: frame.step_name,
                sub: None,
            };
            let route = clause.route.clone();
            return emit_route(emitter, flow, parent_frame, &route, scope, piped);
        }
    }

    // The nested flow's exit.
    if let Some(exit) = &flow.exit
        && exit.name == target.name
    {
        return emit_exit_return(emitter, flow, target, scope, piped);
    }

    if flow.exit.is_none() && emitter.outcomes.contains_key(target.name.as_str()) {
        return emit_outcome_return(emitter, target, scope, piped);
    }

    let step_index = flow.steps.iter().position(|step| step.name == target.name);
    if let Some(step_index) = step_index {
        if piped.is_some() {
            return Err(EmitError::new(
                target.name_span,
                "a piped route must target a workflow outcome — steps carry no payloads",
            ));
        }
        if target.payload.is_some() {
            return Err(EmitError::new(
                target.name_span,
                "routing to a step carries no payload",
            ));
        }
        let Some(region) = flow.plan.region_of_entry(step_index) else {
            return Err(EmitError::new(
                target.name_span,
                format!("`{}` is not a routable step entry", target.name),
            ));
        };
        let args = flow
            .plan
            .region_params(region)
            .iter()
            .map(|name| ident(name))
            .collect::<Vec<_>>()
            .join(", ");
        emitter.line(&format!("{}({args})", flow.step_fn(&target.name)));
        return Ok(());
    }
    Err(EmitError::new(
        target.name_span,
        format!("`{}` names no workflow outcome or step", target.name),
    ))
}

/// Return from a nested flow through its exit: `Ok(payload)` for a subflow
/// outcome, `Ok(<collected binding>)` for a region member flow's close.
fn emit_exit_return(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    target: &RouteTarget,
    scope: &Scope,
    piped: Option<(String, GType)>,
) -> Result<(), EmitError> {
    let Some(exit) = &flow.exit else {
        return Err(EmitError::new(target.name_span, "flow lost its exit"));
    };
    match &exit.kind {
        ExitKind::Region { binding } => {
            if piped.is_some() || target.payload.is_some() {
                return Err(EmitError::new(
                    target.name_span,
                    "routing to a region's `collect` carries no payload — the collect \
                     gathers the per-instance binding",
                ));
            }
            emitter.line(&format!("Ok({})", ident(binding)));
            Ok(())
        }
        ExitKind::Subflow { ty } => {
            let ty = ty.clone();
            let mut prelude = Vec::new();
            let payload = render_payload(emitter, target, &ty, scope, piped, &mut prelude)?;
            flush_prelude(emitter, prelude);
            emitter.line(&format!("Ok({payload})"));
            Ok(())
        }
    }
}

/// Render the payload value a route carries toward a typed destination:
/// constructed named fields, a single value expression, the piped value,
/// the binding named after the destination, or `Nil`.
fn render_payload(
    emitter: &mut Emitter<'_>,
    target: &RouteTarget,
    into: &GType,
    scope: &Scope,
    piped: Option<(String, GType)>,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    if piped.is_some() && target.payload.is_some() {
        return Err(EmitError::new(
            target.span,
            "a piped route carries the piped value as its payload — payload construction is \
             not allowed here (the document did not check cleanly)",
        ));
    }
    if let Some(RoutePayload::Value(value)) = &target.payload {
        return render_arg_for(emitter, value, into, scope, prelude);
    }
    if let Some(RoutePayload::Args(args)) = &target.payload {
        // Constructed payload: the destination type must be a record.
        let Some((gleam_name, record)) = emitter.env.record_of(into) else {
            return Err(EmitError::new(
                target.name_span,
                format!(
                    "outcome `{}` carries {}, which cannot take named payload fields",
                    target.name,
                    emitter.env.gleam_type(into)
                ),
            ));
        };
        let fields = record.fields.clone();
        if fields.is_empty() {
            return Ok(gleam_name);
        }
        let mut rendered = format!("{gleam_name}(");
        for (position, field) in fields.iter().enumerate() {
            if position > 0 {
                rendered.push_str(", ");
            }
            let value = match args.iter().find(|arg| arg.name == field.awl_name) {
                Some(arg) => render_arg_for(emitter, &arg.value, &field.ty, scope, prelude)?,
                None if matches!(emitter.env.resolve(&field.ty), GType::Option(_)) => {
                    "None".to_owned()
                }
                None => {
                    return Err(EmitError::new(
                        target.span,
                        format!(
                            "outcome `{}` misses its required payload field `{}`",
                            target.name, field.awl_name
                        ),
                    ));
                }
            };
            let _ = write!(rendered, "{}: {value}", ident(&field.awl_name));
        }
        rendered.push(')');
        return Ok(rendered);
    }
    if let Some((value, value_ty)) = piped {
        return Ok(wrap_optional(emitter, value, &value_ty, into));
    }
    if let Some(bound_ty) = scope.get(target.name.as_str()) {
        // Bare route picks up the binding named after the destination.
        let value = ident(&target.name);
        return Ok(wrap_optional(emitter, value, &bound_ty.clone(), into));
    }
    if matches!(emitter.env.resolve(into), GType::Nil) {
        return Ok("Nil".to_owned());
    }
    Err(EmitError::new(
        target.name_span,
        format!(
            "bare `route {}` needs a binding named `{}` in scope to pick up",
            target.name, target.name
        ),
    ))
}

/// Return from the workflow with an outcome: `Ok(Ctor(payload))` on the
/// success path, `Error(AwlOutcomeFailure(…))` on the failure path.
fn emit_outcome_return(
    emitter: &mut Emitter<'_>,
    target: &RouteTarget,
    scope: &Scope,
    piped: Option<(String, GType)>,
) -> Result<(), EmitError> {
    let info = emitter.outcomes[target.name.as_str()].clone();
    let mut prelude = Vec::new();
    let payload = render_payload(emitter, target, &info.ty, scope, piped, &mut prelude)?;
    flush_prelude(emitter, prelude);
    match info.direction {
        RouteDirection::Success => {
            let Some(constructor) = &info.constructor else {
                return Err(EmitError::new(
                    target.name_span,
                    "success outcome lost its constructor",
                ));
            };
            emitter.line(&format!("Ok({constructor}({payload}))"));
        }
        RouteDirection::Failure => {
            let to_json = emitter.to_json_fn(&info.ty);
            emitter.line(&format!(
                "Error(awl_error.AwlOutcomeFailure({}, json.to_string({to_json}({payload}))))",
                string_lit(&target.name)
            ));
        }
    }
    Ok(())
}

/// Emit a step's outcome clauses: a single enum `case` when every arm is
/// `when <subject> == <Variant>` over one subject, a `when`-cascade of
/// nested `case … { True | False }` otherwise. Guard-dependent optionality:
/// an arm guarded by `x is present` rebinds `x` unwrapped for its body.
pub(super) fn emit_outcomes(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    frame: Frame<'_>,
    clauses: &[OutcomeClause],
    scope: &Scope,
) -> Result<(), EmitError> {
    if let Some((subject, arms)) = enum_total_form(emitter, clauses, scope) {
        let mut prelude = Vec::new();
        let rendered = render_expr(emitter, &subject, scope, &mut prelude)?;
        if !prelude.is_empty() {
            return Err(EmitError::new(
                subject.span(),
                "indexing inside an outcome guard is not lowerable in the Gleam stopgap",
            ));
        }
        emitter.line(&format!("case {rendered} {{"));
        emitter.indented_try(|this| {
            for (variant, clause) in arms {
                this.line(&format!("{variant} -> {{"));
                this.indented_try(|this| {
                    let route = clause.route.clone();
                    emit_route(this, flow, frame, &route, scope, None)
                })?;
                this.line("}");
            }
            Ok(())
        })?;
        emitter.line("}");
        return Ok(());
    }
    emit_cascade(emitter, flow, frame, clauses, scope)
}

/// `when <subject> == <Variant>` over one common subject, all arms.
fn enum_total_form<'c>(
    emitter: &mut Emitter<'_>,
    clauses: &'c [OutcomeClause],
    scope: &Scope,
) -> Option<(Expr, Vec<(String, &'c OutcomeClause)>)> {
    let mut subject: Option<Expr> = None;
    let mut arms = Vec::new();
    for clause in clauses {
        let Guard::When { expr, .. } = &clause.guard else {
            return None;
        };
        let Expr::Binary {
            left, op, right, ..
        } = expr
        else {
            return None;
        };
        if !matches!(op, BinaryOp::Eq) {
            return None;
        }
        let Expr::Variant { name, .. } = right.as_ref() else {
            return None;
        };
        match &subject {
            None => subject = Some(left.as_ref().clone()),
            Some(existing) => {
                let mut scratch_a = Vec::new();
                let mut scratch_b = Vec::new();
                let a = render_expr(emitter, existing, scope, &mut scratch_a).ok()?;
                let b = render_expr(emitter, left, scope, &mut scratch_b).ok()?;
                if a != b || !scratch_a.is_empty() || !scratch_b.is_empty() {
                    return None;
                }
            }
        }
        arms.push((name.clone(), clause));
    }
    let subject = subject?;
    // The checker proved totality; verify the subject is enum-typed so the
    // Gleam `case` is exhaustive.
    let subject_ty = expr_type(emitter, &subject, scope).ok()?;
    match emitter.env.resolve(&subject_ty) {
        GType::Named(name) => match emitter.env.get(&name) {
            Some(super::types::NamedDef::Enum(variants)) if variants.len() == arms.len() => {
                Some((subject, arms))
            }
            _ => None,
        },
        _ => None,
    }
}

fn emit_cascade(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    frame: Frame<'_>,
    clauses: &[OutcomeClause],
    scope: &Scope,
) -> Result<(), EmitError> {
    let Some((clause, rest)) = clauses.split_first() else {
        return Err(EmitError::new(
            emitter.document.span,
            "outcome clauses ended without an `otherwise` arm — the guards are not provably \
             total in the Gleam stopgap",
        ));
    };
    match &clause.guard {
        Guard::Otherwise { span } => {
            if !rest.is_empty() {
                return Err(EmitError::new(*span, "`otherwise` must be the last arm"));
            }
            emit_arm(emitter, flow, frame, clause, scope, None)
        }
        Guard::When { expr, .. } => {
            let mut prelude = Vec::new();
            let rendered = render_expr(emitter, expr, scope, &mut prelude)?;
            if !prelude.is_empty() {
                return Err(EmitError::new(
                    expr.span(),
                    "indexing inside an outcome guard is not lowerable in the Gleam stopgap",
                ));
            }
            emitter.line(&format!("case {rendered} {{"));
            emitter.indented_try(|this| {
                this.line("True -> {");
                this.indented_try(|this| emit_arm(this, flow, frame, clause, scope, Some(expr)))?;
                this.line("}");
                this.line("False -> {");
                this.indented_try(|this| emit_cascade(this, flow, frame, rest, scope))?;
                this.line("}");
                Ok(())
            })?;
            emitter.line("}");
            Ok(())
        }
    }
}

/// Emit one arm's body: the guard's flow-typing rebind when it applies,
/// then the arm's route.
fn emit_arm(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    frame: Frame<'_>,
    clause: &OutcomeClause,
    scope: &Scope,
    guard: Option<&Expr>,
) -> Result<(), EmitError> {
    let mut arm_scope = scope.clone();
    if let Some(Expr::Predicate {
        subject,
        kind: PredicateKind::Present,
        ..
    }) = guard
        && let Expr::Ref { name, .. } = subject.as_ref()
        && let Some(GType::Option(inner)) = arm_scope.get(name).map(|ty| emitter.env.resolve(ty))
    {
        let rendered = ident(name);
        emitter.line(&format!("let assert Some({rendered}) = {rendered}"));
        arm_scope.insert(name.clone(), (*inner).clone());
    }
    let route = clause.route.clone();
    emit_route(emitter, flow, frame, &route, &arm_scope, None)
}
