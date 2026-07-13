//! Outcome-clause lowering. S17 makes both conditional cascades and enum-total
//! dispatch block tails: every arm ends in a route and no continuation can
//! follow the control construct.

use crate::ast::{BinaryOp, Expr, Guard, OutcomeClause, PredicateKind};
use crate::emitter::{GType, snake};
use crate::spanned::Spanned;

use super::super::ids::Span;
use super::super::ops::{Block, Stmt, Tail, Test};
use super::build::FnPlan;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_expr};
use super::flow::route_tail;

pub(super) fn lower_outcomes(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    clauses: &[OutcomeClause],
    scope: &Scope,
) -> Result<Block, LowerError> {
    if let Some((subject, arms)) = enum_total_form(clauses) {
        let mut stmts = Vec::new();
        let (subject, _) = lower_expr(ctx, &subject, scope, &mut stmts)?;
        let mut lowered = Vec::with_capacity(arms.len());
        for (variant, clause) in arms {
            lowered.push((
                ctx.atom(&snake(&variant)),
                lower_outcome_arm(ctx, plan, clause, scope, None)?,
            ));
        }
        return Ok(Block {
            stmts,
            tail: Tail::SelectEnum {
                subject,
                arms: lowered,
                span: Span::from_source(clauses[0].span),
            },
        });
    }
    lower_outcome_cascade(ctx, plan, clauses, scope)
}

fn enum_total_form(clauses: &[OutcomeClause]) -> Option<(Expr, Vec<(String, &OutcomeClause)>)> {
    let mut subject: Option<Expr> = None;
    let mut arms = Vec::with_capacity(clauses.len());
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
        if subject
            .as_ref()
            .is_some_and(|existing| !same_enum_subject(existing, left))
        {
            return None;
        }
        subject.get_or_insert_with(|| left.as_ref().clone());
        arms.push((name.clone(), clause));
    }
    Some((subject?, arms))
}

fn same_enum_subject(left: &Expr, right: &Expr) -> bool {
    match (left, right) {
        (Expr::Ref { name: a, .. }, Expr::Ref { name: b, .. }) => a == b,
        (
            Expr::Field {
                base: a_base,
                name: a_name,
                ..
            },
            Expr::Field {
                base: b_base,
                name: b_name,
                ..
            },
        ) => a_name == b_name && same_enum_subject(a_base, b_base),
        _ => false,
    }
}

#[derive(Clone)]
enum Decision<'a> {
    Test {
        expr: &'a Expr,
        when_true: Box<Decision<'a>>,
        when_false: Box<Decision<'a>>,
    },
    /// The false side of `x is absent` and the true side of `x is present`
    /// prove `x` is `Some`. Keep that proof on the control edge so a
    /// short-circuited RHS can safely read fields from the payload.
    NarrowPresent {
        name: &'a str,
        span: crate::Span,
        next: Box<Decision<'a>>,
    },
    Arm {
        clause: &'a OutcomeClause,
        guard: Option<&'a Expr>,
    },
    Cascade(&'a [OutcomeClause]),
}

fn lower_outcome_cascade(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    clauses: &[OutcomeClause],
    scope: &Scope,
) -> Result<Block, LowerError> {
    let Some((clause, rest)) = clauses.split_first() else {
        return Err(LowerError::new(
            ctx.emitter.document.span,
            "outcome clauses ended without an `otherwise` arm",
        ));
    };
    match &clause.guard {
        Guard::Otherwise { span } => {
            if !rest.is_empty() {
                return Err(LowerError::new(*span, "`otherwise` must be the last arm"));
            }
            lower_outcome_arm(ctx, plan, clause, scope, None)
        }
        Guard::When { expr, span } => {
            let decision = guard_decision(
                expr,
                Decision::Arm {
                    clause,
                    guard: Some(expr),
                },
                Decision::Cascade(rest),
            );
            // Keep the source clause span on the outer decision. Nested tests
            // carry their own expression spans.
            lower_decision(ctx, plan, &decision, scope, Some(*span))
        }
    }
}

/// Compile boolean source syntax into a control tree rather than an eager
/// value expression. In particular, the right side of `and`/`or` (including
/// every `FieldGet` or call prelude it creates) lives only in the branch where
/// Gleam's `&&`/`||` semantics evaluate it.
fn guard_decision<'a>(
    expr: &'a Expr,
    when_true: Decision<'a>,
    when_false: Decision<'a>,
) -> Decision<'a> {
    match expr {
        Expr::Not { expr, .. } => guard_decision(expr, when_false, when_true),
        Expr::Binary {
            left,
            op: BinaryOp::And,
            right,
            ..
        } => {
            let rhs = guard_decision(right, when_true, when_false.clone());
            let rhs = narrow_short_circuit_rhs(left, PredicateKind::Present, rhs);
            guard_decision(left, rhs, when_false)
        }
        Expr::Binary {
            left,
            op: BinaryOp::Or,
            right,
            ..
        } => {
            let rhs = guard_decision(right, when_true.clone(), when_false);
            let rhs = narrow_short_circuit_rhs(left, PredicateKind::Absent, rhs);
            guard_decision(left, when_true, rhs)
        }
        _ => Decision::Test {
            expr,
            when_true: Box::new(when_true),
            when_false: Box::new(when_false),
        },
    }
}

fn narrow_short_circuit_rhs<'a>(
    left: &'a Expr,
    kind: PredicateKind,
    next: Decision<'a>,
) -> Decision<'a> {
    let Expr::Predicate {
        subject,
        kind: actual,
        span,
    } = left
    else {
        return next;
    };
    let Expr::Ref { name, .. } = subject.as_ref() else {
        return next;
    };
    if *actual == kind {
        Decision::NarrowPresent {
            name,
            span: *span,
            next: Box::new(next),
        }
    } else {
        next
    }
}

fn lower_decision(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    decision: &Decision<'_>,
    scope: &Scope,
    outer_span: Option<crate::Span>,
) -> Result<Block, LowerError> {
    match decision {
        Decision::Test {
            expr,
            when_true,
            when_false,
        } => {
            let mut stmts = Vec::new();
            let (test, _) = lower_expr(ctx, expr, scope, &mut stmts)?;
            Ok(Block {
                stmts,
                tail: Tail::If {
                    test: Test::IsTrue(test),
                    then_block: Box::new(lower_decision(ctx, plan, when_true, scope, None)?),
                    else_block: Box::new(lower_decision(ctx, plan, when_false, scope, None)?),
                    span: Span::from_source(outer_span.unwrap_or_else(|| expr.span())),
                },
            })
        }
        Decision::NarrowPresent { name, span, next } => {
            let mut narrowed = scope.clone();
            let mut stmts = Vec::new();
            narrow_binding(ctx, name, *span, &mut narrowed, &mut stmts);
            let mut block = lower_decision(ctx, plan, next, &narrowed, None)?;
            stmts.append(&mut block.stmts);
            block.stmts = stmts;
            Ok(block)
        }
        Decision::Arm { clause, guard } => lower_outcome_arm(ctx, plan, clause, scope, *guard),
        Decision::Cascade(clauses) => lower_outcome_cascade(ctx, plan, clauses, scope),
    }
}

fn lower_outcome_arm(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    clause: &OutcomeClause,
    scope: &Scope,
    guard: Option<&Expr>,
) -> Result<Block, LowerError> {
    let mut arm_scope = scope.clone();
    let mut stmts = Vec::new();
    narrow_present(ctx, guard, &mut arm_scope, &mut stmts);
    let tail = route_tail(ctx, plan, &clause.route, &arm_scope, None, &mut stmts)?;
    Ok(Block { stmts, tail })
}

fn narrow_present(
    ctx: &mut Ctx<'_>,
    guard: Option<&Expr>,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) {
    let Some(Expr::Predicate {
        subject,
        kind: PredicateKind::Present,
        span,
    }) = guard
    else {
        return;
    };
    let Expr::Ref { name, .. } = subject.as_ref() else {
        return;
    };
    narrow_binding(ctx, name, *span, scope, stmts);
}

fn narrow_binding(
    ctx: &mut Ctx<'_>,
    name: &str,
    span: crate::Span,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) {
    let Some(binding) = scope.get(name).cloned() else {
        return;
    };
    let GType::Option(inner) = ctx.emitter.env.resolve(&binding.ty) else {
        return;
    };
    let dst = ctx.fresh_var();
    stmts.push(Stmt::AssertSome {
        dst,
        option: binding.var,
        span: Span::from_source(span),
    });
    scope.insert(
        name.to_owned(),
        Binding {
            var: dst,
            ty: *inner,
        },
    );
}
