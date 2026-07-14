//! Fork lowering — the MIR twin of the reference `emitter/forks.rs`, to the
//! emitter's parity contract, NOT the checker's broader acceptance:
//!
//! - collection `fork item in expr` (parallel, one unbound ACTION call):
//!   a lifted branch-body fn returning the unrun configured activity value,
//!   dispatched through `workflow.map` (input-order results, engine-owned
//!   fail-fast) `|> map_activity_error`, `TryBind`;
//! - collection `… sequential`: `list.try_fold(items, [], fn(acc, item))`
//!   running each activity in input order, prepending, then `list.reverse` —
//!   joined results are input-ordered;
//! - named fork, homogeneous action branches: source-order activity values in
//!   ONE typed `workflow.all`, destructured by `AssertList` in source order;
//! - named fork, heterogeneous action branches: each branch rides its raw
//!   wrapper twin (wire bytes identical, `Activity(String, String)`) in one
//!   `workflow.all`, and the join decodes each bound position with that
//!   action's return codec and string action name (`awlc.decoded/3`).
//!
//! Everything the reference refuses, we refuse (clean `Unsupported`):
//! multi-statement/bound collection bodies, parallel indexing preludes,
//! named-child branches, non-action named branches. Child collection forks
//! additionally stay refused AT LOWER this increment (the child witness
//! shell does not select yet) — a clean diagnostic, never a backend error.
//!
//! ONE deliberate parity exception (BC-2b-5, recorded in AWL-BC-IR.md): the
//! reference emitter passes call-site config on fork branches through
//! `activity_value` (`emitter/forks.rs:218-229,300-336,351-365`), while
//! direct lowering refuses it with the global BC-2 `call-site config` scope
//! class — full support needs per-key site/declaration merge across the
//! typed and raw call paths and stays deferred with the global marker
//! (`tests/compile.rs::call_site_node_override_yields_the_override`;
//! fork-form pins in `mir/fork_tests.rs`).

use std::collections::BTreeSet;

use crate::ast::{CallStmt, Expr, ForkHeader, ForkStmt, Statement, Step};
use crate::emitter::{Emitter, GType, expr_refs, snake, type_ref_to_g};
use crate::spanned::Spanned;

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{Span, Var};
use super::super::ops::{LiveAfter, Stmt, Tail, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::{activity_value, call_rt, record_new};
use super::build::FnPlan;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_expr};
use super::slots::Slots;

/// The fork-function inventory a document's regions will consume, in the
/// exact traversal order lowering encounters them: statements in written
/// order with the `lower_step` early-stop, descending into loop bodies
/// pre-order (a fork inside a loop body consumes its slot while the loop fn
/// lowers). Only the shapes that lower consume a slot: a collection fork
/// whose sole branch is one unbound ACTION call takes exactly one lifted fn
/// (map body or fold body); named forks build inline and take none; every
/// refused shape errors before consuming.
pub(super) fn count_fork_fns(statements: &[Statement], emitter: &Emitter<'_>) -> u32 {
    let mut count = 0;
    for statement in statements {
        match statement {
            Statement::Fork(fork) => count += fork_fn_count(fork, emitter),
            Statement::Loop(looped) => count += count_fork_fns(&looped.body, emitter),
            Statement::Route(_) => break,
            Statement::Pipe(pipe) if matches!(pipe.end, crate::ast::PipeEnd::Route(_)) => break,
            _ => {}
        }
    }
    count
}

fn fork_fn_count(fork: &ForkStmt, emitter: &Emitter<'_>) -> u32 {
    match &fork.header {
        ForkHeader::Collection { .. } => match single_unbound_call(&fork.body) {
            Some(call) if emitter.actions.contains_key(call.call.name.as_str()) => 1,
            _ => 0,
        },
        ForkHeader::Named => 0,
    }
}

/// Every action a heterogeneous named fork dispatches — these need the raw
/// wrapper twin planned (`build::raw_activity_shell`). Sorted (`BTreeSet`) for
/// deterministic slot order; the traversal mirrors `count_fork_fns`.
pub(super) fn raw_action_inventory(emitter: &Emitter<'_>) -> Vec<String> {
    let mut out = BTreeSet::new();
    for step in &emitter.document.steps {
        collect_raw_actions(&step.body, emitter, &mut out);
    }
    out.into_iter().collect()
}

fn collect_raw_actions(
    statements: &[Statement],
    emitter: &Emitter<'_>,
    out: &mut BTreeSet<String>,
) {
    for statement in statements {
        match statement {
            Statement::Fork(fork) if matches!(fork.header, ForkHeader::Named) => {
                let mut names = Vec::new();
                let mut all_actions = true;
                for branch in &fork.body {
                    match branch {
                        Statement::Call(call)
                            if emitter.actions.contains_key(call.call.name.as_str()) =>
                        {
                            names.push(call.call.name.clone());
                        }
                        _ => {
                            all_actions = false;
                            break;
                        }
                    }
                }
                let heterogeneous = names.len() > 1 && names.iter().any(|name| *name != names[0]);
                if all_actions && heterogeneous {
                    out.extend(names);
                }
            }
            Statement::Loop(looped) => collect_raw_actions(&looped.body, emitter, out),
            Statement::Route(_) => break,
            Statement::Pipe(pipe) if matches!(pipe.end, crate::ast::PipeEnd::Route(_)) => break,
            _ => {}
        }
    }
}

fn single_unbound_call(body: &[Statement]) -> Option<&CallStmt> {
    match body {
        [Statement::Call(call)] if call.bind.is_none() => Some(call),
        _ => None,
    }
}

pub(super) fn lower_fork_stmt(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    fork: &ForkStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    match &fork.header {
        ForkHeader::Collection {
            var,
            collection,
            sequential,
            ..
        } => lower_collection_fork(
            ctx,
            plan,
            step,
            fork,
            var,
            collection,
            *sequential,
            scope,
            stmts,
            slots,
        ),
        ForkHeader::Named => super::fork_named::lower_named_fork(ctx, plan, fork, scope, stmts),
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_collection_fork(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    fork: &ForkStmt,
    var: &str,
    collection: &Expr,
    sequential: bool,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let (call, returns) = collection_branch(ctx, fork, sequential)?;

    // R4: the collection expression evaluates BEFORE fan-out.
    let (items_value, items_ty) = lower_expr(ctx, collection, scope, stmts)?;
    let elem_ty = match ctx.emitter.env.resolve(&items_ty) {
        GType::List(inner) => *inner,
        other => {
            return Err(LowerError::new(
                collection.span(),
                format!(
                    "`fork … in` needs a list, found {}",
                    ctx.emitter.env.gleam_type(&other)
                ),
            ));
        }
    };
    let free = branch_free_names(call, var, scope);
    let (ordinal, self_ref) = slots.forks.take()?;

    let build = ForkBuild {
        plan,
        step,
        call,
        var,
        elem_ty: &elem_ty,
        returns: &returns,
        free: &free,
        host_scope: scope,
        ordinal,
        span: fork.span,
    };
    let saved = ctx.swap_var_counter(0);
    let function = if sequential {
        build_fold_body(ctx, &build)
    } else {
        build_map_body(ctx, &build)
    };
    ctx.swap_var_counter(saved);
    slots.forks.finish(ordinal, MirFn::Flow(function?));

    // Call site: close over the sorted free names, then fan out.
    let span = Span::from_source(fork.span);
    let mut captures = Vec::new();
    for name in &free {
        let binding = scope.get(name).ok_or_else(|| {
            LowerError::new(
                fork.span,
                format!("fork free name `{name}` lost its binding"),
            )
        })?;
        captures.push(Value::Var(binding.var));
    }
    let closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: closure,
        lifted: self_ref,
        captures,
        span,
    });
    let joined = fan_out(ctx, fork, sequential, items_value, closure, stmts);
    if let Some(bind) = &fork.join.bind {
        scope.insert(
            bind.name.clone(),
            Binding {
                var: joined,
                ty: GType::List(Box::new(returns)),
            },
        );
    }
    Ok(())
}

/// The fan-out call site: `workflow.map |> map_activity_error` (parallel) or
/// `list.try_fold(items, [], …)` + `list.reverse` (sequential). Returns the
/// joined, input-ordered list var (R3).
fn fan_out(
    ctx: &mut Ctx<'_>,
    fork: &ForkStmt,
    sequential: bool,
    items_value: Value,
    closure: Var,
    stmts: &mut Vec<Stmt>,
) -> Var {
    let span = Span::from_source(fork.span);
    if sequential {
        // `list.try_fold(items, [], fn(acc, item) { … })`, then reverse: the
        // joined list is input-ordered (R3), the initial accumulator is `[]`.
        let folded_result = call_rt(
            ctx,
            RuntimeFn::LTryFold,
            vec![items_value, Value::Nil, Value::Var(closure)],
            stmts,
            fork.span,
        );
        let folded = ctx.fresh_var();
        stmts.push(Stmt::TryBind {
            dst: folded,
            result: folded_result,
            live_after: LiveAfter::default(),
            span,
        });
        call_rt(
            ctx,
            RuntimeFn::LReverse,
            vec![Value::Var(folded)],
            stmts,
            fork.span,
        )
    } else {
        // `workflow.map(items, fn(item) { <activity> }) |> map_activity_error`:
        // input-order outputs, lowest-ordinal failure, engine cancellation (R2/R3).
        let ran = call_rt(
            ctx,
            RuntimeFn::WfMap,
            vec![items_value, Value::Var(closure)],
            stmts,
            fork.span,
        );
        let mapped = call_rt(
            ctx,
            RuntimeFn::MapActivityError,
            vec![Value::Var(ran)],
            stmts,
            fork.span,
        );
        let bound = ctx.fresh_var();
        stmts.push(Stmt::TryBind {
            dst: bound,
            result: mapped,
            live_after: LiveAfter::default(),
            span,
        });
        bound
    }
}

/// The reference stopgap gate for a collection fork body: exactly one
/// unbound ACTION call — everything else refuses with the emitter's
/// diagnostic class (multi-statement/bound bodies, call-site config, child
/// fan-out, parallel indexing preludes).
fn collection_branch<'f>(
    ctx: &Ctx<'_>,
    fork: &'f ForkStmt,
    sequential: bool,
) -> Result<(&'f crate::ast::Call, GType), LowerError> {
    let Some(branch) = single_unbound_call(&fork.body) else {
        // The reference stopgap: one unbound call per item, nothing else.
        return Err(LowerError::unsupported(
            "a collection fork body beyond one unbound call",
            fork.span,
        ));
    };
    if branch.config.is_some() {
        return Err(LowerError::unsupported("call-site config", branch.span));
    }
    let call = &branch.call;
    if ctx.emitter.children.contains_key(call.name.as_str()) {
        // R7: the child witness shell does not select yet, so the child
        // fan-out keeps refusing AT LOWER — a clean diagnostic, never a
        // dirty backend error.
        return Err(LowerError::unsupported(
            "child collection fork",
            call.name_span,
        ));
    }
    let Some(&(_, decl)) = ctx.emitter.actions.get(call.name.as_str()) else {
        return Err(LowerError::new(
            call.name_span,
            format!(
                "`{}` names neither a declared action nor a child workflow",
                call.name
            ),
        ));
    };
    if !sequential && args_contain_index(call) {
        // The reference refuses indexing preludes inside a PARALLEL branch.
        return Err(LowerError::unsupported(
            "indexing inside a parallel fork branch",
            call.span,
        ));
    }
    Ok((call, type_ref_to_g(&decl.returns)))
}

struct ForkBuild<'a> {
    plan: &'a FnPlan,
    step: &'a Step,
    call: &'a crate::ast::Call,
    var: &'a str,
    elem_ty: &'a GType,
    returns: &'a GType,
    free: &'a [String],
    host_scope: &'a Scope,
    ordinal: usize,
    span: crate::Span,
}

/// The shared closure frame: `item` (or `acc, item`) params plus the sorted
/// free captures, with the branch scope holding the item and free names.
fn closure_frame(
    ctx: &mut Ctx<'_>,
    build: &ForkBuild<'_>,
    leading: &[(Var, TyDesc)],
) -> Result<(Vec<Var>, Vec<TyDesc>, Scope), LowerError> {
    let mut params = Vec::new();
    let mut param_tys = Vec::new();
    for (var, ty) in leading {
        params.push(*var);
        param_tys.push(ty.clone());
    }
    let mut fn_scope: Scope = Scope::new();
    for name in build.free {
        let host = build.host_scope.get(name).ok_or_else(|| {
            LowerError::new(
                build.span,
                format!("fork free name `{name}` lost its binding"),
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
    Ok((params, param_tys, fn_scope))
}

fn fork_fn(
    build: &ForkBuild<'_>,
    frame: (Vec<Var>, Vec<TyDesc>),
    ret_ty: TyDesc,
    body: super::super::ops::Block,
) -> FlowFn {
    let (params, param_tys) = frame;
    FlowFn {
        origin: FnOrigin::Fork {
            step: build.step.name.clone(),
            index: u32::try_from(build.ordinal).unwrap_or(u32::MAX),
        },
        name: format!("{}_fork_{}", snake(&build.step.name), build.ordinal),
        params,
        param_tys,
        ret_ty,
        body,
        span: Span::from_source(build.span),
        degraded_parallel: false,
    }
}

/// The parallel branch body: `fn(item, free…) -> Activity(input, return)` —
/// the UNRUN configured activity value `workflow.map` dispatches (R1: the
/// action routing; children never reach here).
fn build_map_body(ctx: &mut Ctx<'_>, build: &ForkBuild<'_>) -> Result<FlowFn, LowerError> {
    let item = ctx.fresh_var();
    let elem_desc = ctx.tydesc(build.elem_ty);
    let (params, param_tys, mut fn_scope) = closure_frame(ctx, build, &[(item, elem_desc)])?;
    fn_scope.insert(
        build.var.to_owned(),
        Binding {
            var: item,
            ty: build.elem_ty.clone(),
        },
    );
    let mut stmts = Vec::new();
    let queued = activity_value(
        ctx, build.plan, build.call, None, &fn_scope, &mut stmts, false,
    )?;
    let input_name = ctx.emitter.action_inputs[build.call.name.as_str()].clone();
    let ret_ty = TyDesc::Activity(
        Box::new(TyDesc::Custom {
            module: ctx.module_name.clone(),
            name: input_name,
            params: Vec::new(),
        }),
        Box::new(ctx.tydesc(build.returns)),
    );
    Ok(fork_fn(
        build,
        (params, param_tys),
        ret_ty,
        super::super::ops::Block {
            stmts,
            tail: Tail::Return(Value::Var(queued)),
        },
    ))
}

/// The sequential fold body: `fn(acc, item, free…) -> Result(List, AwlError)`
/// running the activity durably (`workflow.run |> map_activity_error`), then
/// `Ok([item, ..acc])` — the reference's exact per-item order (R3/R4).
fn build_fold_body(ctx: &mut Ctx<'_>, build: &ForkBuild<'_>) -> Result<FlowFn, LowerError> {
    let acc = ctx.fresh_var();
    let item = ctx.fresh_var();
    let elem_desc = ctx.tydesc(build.elem_ty);
    let acc_desc = TyDesc::List(Box::new(ctx.tydesc(build.returns)));
    let (params, param_tys, mut fn_scope) =
        closure_frame(ctx, build, &[(acc, acc_desc.clone()), (item, elem_desc)])?;
    fn_scope.insert(
        build.var.to_owned(),
        Binding {
            var: item,
            ty: build.elem_ty.clone(),
        },
    );
    let mut stmts = Vec::new();
    let queued = activity_value(
        ctx, build.plan, build.call, None, &fn_scope, &mut stmts, false,
    )?;
    let ran = call_rt(
        ctx,
        RuntimeFn::WfRun,
        vec![Value::Var(queued)],
        &mut stmts,
        build.call.name_span,
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapActivityError,
        vec![Value::Var(ran)],
        &mut stmts,
        build.call.name_span,
    );
    let span = Span::from_source(build.span);
    let bound = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: bound,
        result: mapped,
        live_after: LiveAfter::default(),
        span,
    });
    let consed = ctx.fresh_var();
    stmts.push(Stmt::ListPrepend {
        dst: consed,
        head: Value::Var(bound),
        tail: Value::Var(acc),
        span,
    });
    let ok = ctx.atom("ok");
    let ok_var = record_new(ctx, ok, vec![Value::Var(consed)], &mut stmts);
    Ok(fork_fn(
        build,
        (params, param_tys),
        TyDesc::Result(Box::new(acc_desc), Box::new(TyDesc::AwlError)),
        super::super::ops::Block {
            stmts,
            tail: Tail::Return(Value::Var(ok_var)),
        },
    ))
}

/// Branch-call refs beyond the loop var, restricted to names the call site
/// can supply — sorted (`BTreeSet`) so capture order is deterministic (R4).
fn branch_free_names(call: &crate::ast::Call, var: &str, scope: &Scope) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for arg in &call.args {
        expr_refs(&arg.value, &mut refs);
    }
    refs.remove(var);
    refs.into_iter()
        .filter(|name| scope.contains_key(name))
        .collect()
}

fn args_contain_index(call: &crate::ast::Call) -> bool {
    call.args.iter().any(|arg| expr_contains_index(&arg.value))
}

fn expr_contains_index(expr: &Expr) -> bool {
    match expr {
        Expr::Index { .. } => true,
        Expr::Field { base, .. } | Expr::Not { expr: base, .. } => expr_contains_index(base),
        Expr::Binary { left, right, .. } => expr_contains_index(left) || expr_contains_index(right),
        Expr::Predicate { subject, .. } => expr_contains_index(subject),
        Expr::Record { args, .. } => args.iter().any(|arg| expr_contains_index(&arg.value)),
        Expr::List { items, .. } => items.iter().any(expr_contains_index),
        _ => false,
    }
}
