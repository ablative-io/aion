//! Substep-chain lowering: a Gleam function per substep, sibling routes as
//! tail calls, and the parent's outcome clauses evaluated inline at the
//! chain's end (the substep group's boundary).

use crate::ast::{Statement, Step};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::Scope;
use super::graph::{Plan, body_ends_in_route, falls_through};
use super::loops::statement_defs;
use super::names::{ident, snake};
use super::outcomes::emit_outcomes;
use super::steps::{
    Frame, annotated_params, lower_statements, render_defs_tuple, scope_from_params,
};

/// Emit one substep chain: a function per substep, the parent's outcomes
/// evaluated inline at the chain's end.
pub(super) fn emit_sub_chain(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
    parent_index: usize,
    parent: &Step,
    split: usize,
) -> Result<(), EmitError> {
    let count = parent.body.len() - split;
    for position in 0..count {
        let Statement::SubStep(sub) = &parent.body[split + position] else {
            continue;
        };
        let params = plan.sub_params(parent_index, position).to_vec();
        let output = emitter.output_type();
        let mut scope = scope_from_params(emitter, &params, sub)?;
        let rendered_params = annotated_params(emitter, &params, &scope);
        let frame = Frame {
            step_name: &sub.name,
            sub: Some((parent_index, split)),
        };
        emitter.line(&format!(
            "fn sub_{}_{}({rendered_params}) -> Result({output}, AwlError) {{",
            snake(&parent.name),
            snake(&sub.name)
        ));
        let chain = SubChain {
            parent_index,
            parent,
            split,
        };
        emitter.indented_try(|this| {
            if sub.on_failure.is_some() {
                return emit_sub_with_failure(this, plan, chain, position, sub, frame, &mut scope);
            }
            lower_statements(this, plan, frame, &sub.body, &mut scope, false)?;
            emit_sub_tail(this, plan, chain, position, sub, frame, &mut scope)
        })?;
        emitter.line("}");
        emitter.blank();
    }
    Ok(())
}

/// A substep with `on failure`: the body runs in an attempt closure whose
/// error arm carries the compensation.
fn emit_sub_with_failure(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
    chain: SubChain<'_>,
    position: usize,
    sub: &Step,
    frame: Frame<'_>,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let Some(on_failure) = &sub.on_failure else {
        return Err(EmitError::new(sub.name_span, "substep lost its handler"));
    };
    if body_ends_in_route(&sub.body) {
        return Err(EmitError::new(
            sub.name_span,
            format!(
                "substep `{}` combines `on failure` with a body-terminal route — the Gleam \
                 stopgap cannot lower that",
                sub.name
            ),
        ));
    }
    let mut defs = std::collections::BTreeSet::new();
    statement_defs(&sub.body, &mut defs);
    let defs: Vec<String> = defs.into_iter().collect();
    let mut attempt_scope = scope.clone();
    emitter.line("let awl_attempt = fn() {");
    emitter.indented_try(|this| {
        lower_statements(this, plan, frame, &sub.body, &mut attempt_scope, false)?;
        this.line(&format!("Ok({})", render_defs_tuple(&defs)));
        Ok(())
    })?;
    emitter.line("}");
    emitter.line("case awl_attempt() {");
    emitter.indented_try(|this| {
        this.line(&format!("Ok({}) -> {{", render_defs_tuple(&defs)));
        this.indented_try(|this| {
            for name in &defs {
                if let Some(ty) = attempt_scope.get(name) {
                    scope.insert(name.clone(), ty.clone());
                }
            }
            emit_sub_tail(this, plan, chain, position, sub, frame, scope)
        })?;
        this.line("}");
        this.line("Error(_) -> {");
        this.indented_try(|this| {
            let mut compensation_scope = scope.clone();
            lower_statements(
                this,
                plan,
                frame,
                &on_failure.body,
                &mut compensation_scope,
                true,
            )
        })?;
        this.line("}");
        Ok(())
    })?;
    emitter.line("}");
    Ok(())
}

/// Identity of one substep chain: which parent step and where its trailing
/// substep block starts.
#[derive(Clone, Copy)]
struct SubChain<'a> {
    parent_index: usize,
    parent: &'a Step,
    split: usize,
}

/// A substep's tail: its own outcomes, an emitted terminal route, the next
/// sibling, or the parent's outcome clauses at the chain end.
fn emit_sub_tail(
    emitter: &mut Emitter<'_>,
    plan: &Plan,
    chain: SubChain<'_>,
    position: usize,
    sub: &Step,
    frame: Frame<'_>,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let SubChain {
        parent_index,
        parent,
        split,
    } = chain;
    if !sub.outcomes.is_empty() {
        return emit_outcomes(emitter, plan, frame, &sub.outcomes, scope);
    }
    if body_ends_in_route(&sub.body) {
        return Ok(());
    }
    if !falls_through(sub) {
        return Ok(());
    }
    let next_position = position + 1;
    if split + next_position < parent.body.len() {
        let Statement::SubStep(next) = &parent.body[split + next_position] else {
            return Err(EmitError::new(sub.name_span, "substep block mis-shaped"));
        };
        let args = plan
            .sub_params(parent_index, next_position)
            .iter()
            .map(|name| ident(name))
            .collect::<Vec<_>>()
            .join(", ");
        emitter.line(&format!(
            "sub_{}_{}({args})",
            snake(&parent.name),
            snake(&next.name)
        ));
        return Ok(());
    }
    // Chain end: the parent's outcomes are the boundary. Their routes
    // resolve at the parent's level.
    let parent_frame = Frame {
        step_name: &parent.name,
        sub: None,
    };
    emit_outcomes(emitter, plan, parent_frame, &parent.outcomes, scope)
}
