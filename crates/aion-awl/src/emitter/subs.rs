//! Substep-chain lowering: a Gleam function per substep, sibling routes as
//! tail calls, and the parent's outcome clauses evaluated inline at the
//! chain's end (the substep group's boundary). `on failure` on a substep
//! rides the shared attempt-closure shape in `steps::emit_with_failure`,
//! body-terminal routes included.

use crate::ast::{Statement, Step};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::Scope;
use super::graph::{body_ends_in_route, falls_through};
use super::names::ident;
use super::outcomes::emit_outcomes;
use super::steps::{
    FlowCtx, Frame, annotated_params, emit_with_failure, lower_statements, scope_from_params,
};

/// Emit one substep chain: a function per substep, the parent's outcomes
/// evaluated inline at the chain's end.
pub(super) fn emit_sub_chain(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    parent_index: usize,
    parent: &Step,
    split: usize,
) -> Result<(), EmitError> {
    let count = parent.body.len() - split;
    for position in 0..count {
        let Statement::SubStep(sub) = &parent.body[split + position] else {
            continue;
        };
        let params = flow.plan.sub_params(parent_index, position).to_vec();
        let output = flow.output.clone();
        let mut scope = scope_from_params(emitter, &params, sub)?;
        let rendered_params = annotated_params(emitter, &params, &scope);
        let frame = Frame {
            step_name: &sub.name,
            sub: Some((parent_index, split)),
        };
        emitter.line(&format!(
            "fn {}({rendered_params}) -> Result({output}, awl_error.AwlError) {{",
            flow.sub_fn(&parent.name, &sub.name)
        ));
        let chain = SubChain {
            parent_index,
            parent,
            split,
        };
        emitter.indented_try(|this| {
            if let Some(on_failure) = &sub.on_failure {
                let on_failure_body = on_failure.body.clone();
                return emit_with_failure(
                    this,
                    flow,
                    frame,
                    &sub.body,
                    &on_failure_body,
                    &mut scope,
                    &mut |this, scope| {
                        emit_sub_tail(this, flow, chain, position, sub, frame, scope)
                    },
                );
            }
            lower_statements(this, flow, frame, &sub.body, &mut scope, false)?;
            emit_sub_tail(this, flow, chain, position, sub, frame, &mut scope)
        })?;
        emitter.line("}");
        emitter.blank();
    }
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
    flow: &FlowCtx<'_>,
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
        return emit_outcomes(emitter, flow, frame, &sub.outcomes, scope);
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
        let args = flow
            .plan
            .sub_params(parent_index, next_position)
            .iter()
            .map(|name| ident(name))
            .collect::<Vec<_>>()
            .join(", ");
        emitter.line(&format!("{}({args})", flow.sub_fn(&parent.name, &next.name)));
        return Ok(());
    }
    // Chain end: the parent's outcomes are the boundary. Their routes
    // resolve at the parent's level.
    let parent_frame = Frame {
        step_name: &parent.name,
        sub: None,
    };
    emit_outcomes(emitter, flow, parent_frame, &parent.outcomes, scope)
}
