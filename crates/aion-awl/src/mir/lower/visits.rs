//! The `max … visits` prologue: a bounded step increments its language-owned
//! counter at entry and refuses the visit past the bound with the spanned
//! `AwlVisitsExceeded` runtime failure (the reference
//! `emitter/steps.rs::emit_visits_prologue`); the counter identity is the
//! persisted allocation made while shaping (`flowshape::rewrite_visits`).

use crate::ast::{MaxVisits, Step};
use crate::emitter::{GType, visits_counter};

use super::super::ids::Span;
use super::super::ops::{Block, CmpOp, Stmt, Tail, Test, Value};
use super::activity::record_new;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_expr};

/// The persisted visit-counter name of a bounded step, as a lowering result.
pub(super) fn counter_name(ctx: &Ctx<'_>, step: &Step) -> Result<String, LowerError> {
    visits_counter(step, &ctx.emitter.generated_names)
        .map_err(|error| LowerError::new(error.span, error.message))
}

/// Build the visit-bound prologue: the counter increment (rebinding the name
/// for the rest of the step), the once-lowered bound expression, and the
/// past-the-bound arm's `Error(AwlVisitsExceeded(message))` block.
pub(super) fn visits_prologue(
    ctx: &mut Ctx<'_>,
    step: &Step,
    max_visits: &MaxVisits,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(Test, Box<Block>), LowerError> {
    if super::expr::expr_contains_index(&max_visits.bound) {
        // Emitter parity: an indexing prelude inside the bound is refused
        // (`emitter/steps.rs::emit_visits_prologue`).
        return Err(LowerError::unsupported(
            "indexing inside a `max … visits` bound",
            max_visits.span,
        ));
    }
    let counter = counter_name(ctx, step)?;
    let entry = scope
        .get(&counter)
        .ok_or_else(|| {
            LowerError::new(
                step.name_span,
                format!(
                    "visit counter `{counter}` was not threaded into step `{}` — the shared \
                     plan and the chain boundaries disagree",
                    step.name
                ),
            )
        })?
        .var;
    let span = Span::from_source(max_visits.span);
    let bumped = ctx.fresh_var();
    stmts.push(Stmt::Increment {
        dst: bumped,
        src: entry,
        span,
    });
    scope.insert(
        counter,
        Binding {
            var: bumped,
            ty: GType::Int,
        },
    );
    let (bound, _) = lower_expr(ctx, &max_visits.bound, scope, stmts)?;
    let message = format!(
        "step `{}` exceeded its `max … visits` bound at line {}, column {}",
        step.name, max_visits.span.line, max_visits.span.column
    );
    let message_lit = ctx.binary(&message);
    let mut error_stmts = Vec::new();
    let exceeded = ctx.atom("awl_visits_exceeded");
    let failure = record_new(
        ctx,
        exceeded,
        vec![Value::Lit(message_lit)],
        &mut error_stmts,
    );
    let error_atom = ctx.atom("error");
    let error = record_new(ctx, error_atom, vec![Value::Var(failure)], &mut error_stmts);
    Ok((
        Test::Cmp {
            op: CmpOp::Gt,
            lhs: Value::Var(bumped),
            rhs: bound,
        },
        Box::new(Block {
            stmts: error_stmts,
            tail: Tail::Return(Value::Var(error)),
        }),
    ))
}
