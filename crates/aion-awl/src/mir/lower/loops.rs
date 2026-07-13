//! Bounded-loop lowering to a self-tail-calling `FlowFn(Loop)` — the MIR twin
//! of the reference `emitter/loops.rs::lower_loop`. The generated function
//! takes `(threaded, awl_count, awl_max, free…)`, runs the body (at least
//! once — the checks are post-body), increments the hidden count, then tests
//! `until` FIRST and the ceiling SECOND, exactly the reference order: a
//! condition that turns true on the ceiling pass is success-side state. Both
//! exits return `Ok(threaded)` — or `Ok(#(threaded, count))` when `counting`
//! names the counter — so `max` exhaustion is distinguished by the step's
//! outcome clauses, never by an error. The call site `CallLocal`s the
//! reserved self ref with count 0 and the once-evaluated seed/ceiling,
//! `TryBind`s, and (when counted) destructures the pair; a backward route
//! that re-enters the owning step therefore re-evaluates the seed and resets
//! the count — the reference emitter's exact re-entry behavior (the spec
//! leaves re-entry unstated; we pin the implementation, recorded in
//! AWL-BC-IR.md).

use crate::ast::{LoopStmt, Statement, Step};
use crate::emitter::{
    GType, expr_refs, first_route_span, snake, statement_defs, statements_expr_refs,
};
use crate::spanned::Spanned;

use std::collections::BTreeSet;

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{FnRef, Span};
use super::super::ops::{Block, CmpOp, LiveAfter, Stmt, Tail, Test, Value};
use super::super::tydesc::TyDesc;
use super::activity::record_new;
use super::build::FnPlan;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_expr};
use super::flow::lower_statement;
use super::outcome::lower_condition;

/// The reserved loop function slots (skeleton-planned, pre-order) and the
/// bodies built while regions lower. `FnRef(n)` is literally `functions[n]`,
/// so the built list is appended only after every chain function, in
/// reservation order.
pub(super) struct LoopSlots {
    refs: Vec<FnRef>,
    next: usize,
    built: Vec<Option<MirFn>>,
}

impl LoopSlots {
    pub(super) fn new(refs: Vec<FnRef>) -> Self {
        let built = refs.iter().map(|_| None).collect();
        Self {
            refs,
            next: 0,
            built,
        }
    }

    /// Append the built loop functions at their reserved indices. Every
    /// reserved slot must have been consumed — a hole would misalign every
    /// later `FnRef`.
    pub(super) fn append_into(self, functions: &mut Vec<MirFn>) -> Result<(), LowerError> {
        for (ordinal, slot) in self.built.into_iter().enumerate() {
            let function = slot.ok_or_else(|| LowerError::Planning {
                message: format!("reserved loop slot {ordinal} was never lowered"),
            })?;
            if self.refs[ordinal].0 as usize != functions.len() {
                return Err(LowerError::Planning {
                    message: format!("loop slot {ordinal} misaligned with its reserved ref"),
                });
            }
            functions.push(function);
        }
        Ok(())
    }
}

/// The loop inventory a document's regions will consume, in the exact
/// traversal order lowering encounters them: regions in plan order, chain
/// steps in layer order, statements in written order with the same
/// early-stop as `lower_step` (nothing after a terminal route lowers), and
/// pre-order into nested loop bodies (a loop takes its ordinal before its
/// body lowers — the reference `loop_counter` discipline). Fork and substep
/// bodies are deliberately not traversed while those shapes refuse before
/// consuming slots; their lowering increments must extend this inventory in
/// the same change.
pub(super) fn count_loops(statements: &[Statement]) -> u32 {
    let mut count = 0;
    for statement in statements {
        match statement {
            Statement::Loop(looped) => {
                count += 1 + count_loops(&looped.body);
            }
            Statement::Route(_) => break,
            Statement::Pipe(pipe) if matches!(pipe.end, crate::ast::PipeEnd::Route(_)) => break,
            _ => {}
        }
    }
    count
}

pub(super) fn lower_loop_stmt(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    looped: &LoopStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut LoopSlots,
) -> Result<(), LowerError> {
    preflight(looped, scope)?;
    let Some(max) = &looped.max else {
        return Err(LowerError::new(
            looped.span,
            "an unbounded loop (no `max`) is illegal until implicit continue-as-new lands",
        ));
    };

    // Seed and ceiling evaluate once, at the call site (reference order).
    let (seed_value, seed_ty) = lower_expr(ctx, &looped.seed, scope, stmts)?;
    let (max_value, _) = lower_expr(ctx, &max.expr, scope, stmts)?;
    let free = free_names(looped, scope);

    let ordinal = slots.next;
    slots.next += 1;
    let self_ref = *slots
        .refs
        .get(ordinal)
        .ok_or_else(|| LowerError::Planning {
            message: "loop encountered beyond the reserved inventory".to_owned(),
        })?;

    let build = LoopBuild {
        plan,
        step,
        looped,
        host_scope: scope,
        seed_ty: &seed_ty,
        free: &free,
        self_ref,
        ordinal,
    };
    let function = lower_loop_fn(ctx, &build, slots)?;
    slots.built[ordinal] = Some(MirFn::Flow(function));

    // Call site: count starts at 0; the worker never sees it.
    let mut args = vec![seed_value, Value::Int(0), max_value];
    for name in &free {
        let binding = scope.get(name).ok_or_else(|| {
            LowerError::new(
                looped.span,
                format!("loop free name `{name}` lost its binding"),
            )
        })?;
        args.push(Value::Var(binding.var));
    }
    let span = Span::from_source(looped.span);
    let result = ctx.fresh_var();
    stmts.push(Stmt::CallLocal {
        dst: Some(result),
        callee: self_ref,
        args,
        live_after: LiveAfter::default(),
        span,
    });
    let bound = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: bound,
        result,
        live_after: LiveAfter::default(),
        span,
    });
    if let Some(counter) = &looped.counter {
        // #(value, count): untagged, so elements sit at 0 and 1.
        let value_var = ctx.fresh_var();
        stmts.push(Stmt::FieldGet {
            dst: value_var,
            base: Value::Var(bound),
            index: 0,
            span,
        });
        let count_var = ctx.fresh_var();
        stmts.push(Stmt::FieldGet {
            dst: count_var,
            base: Value::Var(bound),
            index: 1,
            span,
        });
        scope.insert(
            looped.var.clone(),
            Binding {
                var: value_var,
                ty: seed_ty,
            },
        );
        scope.insert(
            counter.name.clone(),
            Binding {
                var: count_var,
                ty: GType::Int,
            },
        );
    } else {
        scope.insert(
            looped.var.clone(),
            Binding {
                var: bound,
                ty: seed_ty,
            },
        );
    }
    Ok(())
}

/// The reference `loop_preflight` refusals, message-for-message: the loop
/// must be bounded, the body may not route, and the ceiling must be
/// loop-invariant (checked-document discipline is the checker's job; these
/// are the defensive backstops for `lower` on an unchecked document).
fn preflight(looped: &LoopStmt, scope: &Scope) -> Result<(), LowerError> {
    if let Some(span) = first_route_span(&looped.body) {
        return Err(LowerError::new(
            span,
            "a `route` inside a `loop` body is illegal (`check` refuses it) — a loop \
             exits through `until`/`max`; route from the loop-carrying step's outcome \
             clauses",
        ));
    }
    if let Some(max) = &looped.max {
        let mut max_refs = BTreeSet::new();
        expr_refs(&max.expr, &mut max_refs);
        if let Some(name) = max_refs.iter().find(|name| !scope.contains_key(*name)) {
            return Err(LowerError::new(
                max.expr.span(),
                format!(
                    "`max` is evaluated once, before the loop runs — `{name}` is not bound before \
                     the loop (the ceiling must be an expression over inputs and prior bindings)"
                ),
            ));
        }
    }
    Ok(())
}

/// The reference `loop_free_names`: body/`until` refs beyond the loop locals
/// and body defs, restricted to names the call site can actually supply.
fn free_names(looped: &LoopStmt, scope: &Scope) -> Vec<String> {
    let mut refs = BTreeSet::new();
    statements_expr_refs(&looped.body, &mut refs);
    if let Some(until) = &looped.until {
        expr_refs(&until.expr, &mut refs);
    }
    let mut defs = BTreeSet::new();
    statement_defs(&looped.body, &mut defs);
    refs.remove(&looped.var);
    if let Some(counter) = &looped.counter {
        refs.remove(&counter.name);
    }
    refs.into_iter()
        .filter(|name| !defs.contains(name) && scope.contains_key(name))
        .collect()
}

struct LoopBuild<'a> {
    plan: &'a FnPlan,
    step: &'a Step,
    looped: &'a LoopStmt,
    host_scope: &'a Scope,
    seed_ty: &'a GType,
    free: &'a [String],
    self_ref: FnRef,
    ordinal: usize,
}

#[derive(Clone, Copy)]
struct LoopControl {
    rebound: super::super::ids::Var,
    new_count: super::super::ids::Var,
    ceiling: super::super::ids::Var,
}

fn lower_loop_fn(
    ctx: &mut Ctx<'_>,
    build: &LoopBuild<'_>,
    slots: &mut LoopSlots,
) -> Result<FlowFn, LowerError> {
    let saved = ctx.swap_var_counter(0);
    let result = build_loop_fn(ctx, build, slots);
    ctx.swap_var_counter(saved);
    result
}

fn build_loop_fn(
    ctx: &mut Ctx<'_>,
    build: &LoopBuild<'_>,
    slots: &mut LoopSlots,
) -> Result<FlowFn, LowerError> {
    let plan = build.plan;
    let step = build.step;
    let looped = build.looped;
    let host_scope = build.host_scope;
    let seed_ty = build.seed_ty;
    let free = build.free;
    let ordinal = build.ordinal;
    let seed_desc = ctx.tydesc(seed_ty);
    let threaded = ctx.fresh_var();
    let entry_count = ctx.fresh_var();
    let ceiling = ctx.fresh_var();
    let mut params = vec![threaded, entry_count, ceiling];
    let mut param_tys = vec![seed_desc.clone(), TyDesc::Int, TyDesc::Int];
    let mut fn_scope: Scope = Scope::new();
    fn_scope.insert(
        looped.var.clone(),
        Binding {
            var: threaded,
            ty: seed_ty.clone(),
        },
    );
    // The counter is NOT in scope: the checker introduces it only after the
    // loop (outcomes-and-later visibility); the hidden count param carries it.
    for name in free {
        let host = host_scope.get(name).ok_or_else(|| {
            LowerError::new(
                looped.span,
                format!("loop free name `{name}` lost its binding"),
            )
        })?;
        let var = ctx.fresh_var();
        params.push(var);
        param_tys.push(ctx.tydesc(&host.ty));
        fn_scope.insert(
            name.clone(),
            Binding {
                var,
                ty: host.ty.clone(),
            },
        );
    }

    // Body first: the loop is post-test, so even `max <= 0` runs one pass.
    let mut stmts = Vec::new();
    for statement in &looped.body {
        if lower_statement(ctx, plan, step, statement, &mut fn_scope, &mut stmts, slots)?.is_some()
        {
            return Err(LowerError::new(
                looped.span,
                "a `route` inside a `loop` body is illegal (`check` refuses it) — a loop \
                 exits through `until`/`max`; route from the loop-carrying step's outcome \
                 clauses",
            ));
        }
    }
    let rebound = fn_scope
        .get(&looped.var)
        .ok_or_else(|| LowerError::new(looped.span, "the loop body dropped the threaded binding"))?
        .var;
    let span = Span::from_source(looped.span);
    let new_count = ctx.fresh_var();
    stmts.push(Stmt::Increment {
        dst: new_count,
        src: entry_count,
        span,
    });

    let tail_block = control_tail(
        ctx,
        build,
        &fn_scope,
        LoopControl {
            rebound,
            new_count,
            ceiling,
        },
    )?;
    stmts.extend(tail_block.stmts);

    let inner = if looped.counter.is_some() {
        TyDesc::Tuple(vec![seed_desc, TyDesc::Int])
    } else {
        seed_desc
    };
    Ok(FlowFn {
        origin: FnOrigin::Loop {
            step: step.name.clone(),
            index: u32::try_from(ordinal).unwrap_or(u32::MAX),
        },
        name: format!("{}_loop_{}", snake(&step.name), ordinal),
        params,
        param_tys,
        ret_ty: TyDesc::Result(Box::new(inner), Box::new(TyDesc::AwlError)),
        body: Block {
            stmts,
            tail: tail_block.tail,
        },
        span,
        degraded_parallel: false,
    })
}

/// The post-body control tree: `until` FIRST (through the shared
/// short-circuit decision builder), the ceiling SECOND, recursion last. One
/// exit block is cloned into the until-true and ceiling-hit arms —
/// sibling-exclusive, so shared defs are single-def-legal. `until` reads the
/// CURRENT pass's binding (`fn_scope` post-body); a missing `until`
/// (unchecked document) falls through to the bound check alone, exactly like
/// the reference.
fn control_tail(
    ctx: &mut Ctx<'_>,
    build: &LoopBuild<'_>,
    fn_scope: &Scope,
    control: LoopControl,
) -> Result<Block, LowerError> {
    let plan = build.plan;
    let looped = build.looped;
    let free = build.free;
    let self_ref = build.self_ref;
    let LoopControl {
        rebound,
        new_count,
        ceiling,
    } = control;
    let span = Span::from_source(looped.span);
    let mut exit_stmts = Vec::new();
    let exit_value = if looped.counter.is_some() {
        let pair = ctx.fresh_var();
        exit_stmts.push(Stmt::TupleNew {
            dst: pair,
            items: vec![Value::Var(rebound), Value::Var(new_count)],
            span,
        });
        pair
    } else {
        rebound
    };
    let ok = ctx.atom("ok");
    let ok_var = record_new(ctx, ok, vec![Value::Var(exit_value)], &mut exit_stmts);
    let exit = Block {
        stmts: exit_stmts,
        tail: Tail::Return(Value::Var(ok_var)),
    };

    let mut recurse_args = vec![
        Value::Var(rebound),
        Value::Var(new_count),
        Value::Var(ceiling),
    ];
    for name in free {
        let binding = fn_scope.get(name).ok_or_else(|| {
            LowerError::new(
                looped.span,
                format!("loop free name `{name}` lost its binding"),
            )
        })?;
        recurse_args.push(Value::Var(binding.var));
    }
    let bound_check = Block {
        stmts: Vec::new(),
        tail: Tail::If {
            test: Test::Cmp {
                op: CmpOp::Ge,
                lhs: Value::Var(new_count),
                rhs: Value::Var(ceiling),
            },
            then_block: Box::new(exit.clone()),
            else_block: Box::new(Block {
                stmts: Vec::new(),
                tail: Tail::TailLocal {
                    callee: self_ref,
                    args: recurse_args,
                },
            }),
            span,
        },
    };
    match &looped.until {
        Some(tail) => lower_condition(ctx, plan, &tail.expr, fn_scope, &exit, &bound_check),
        None => Ok(bound_check),
    }
}
