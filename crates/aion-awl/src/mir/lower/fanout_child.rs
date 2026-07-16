//! Child fan-out tracks — the MIR twin of `emitter/child_fanout.rs` and the
//! child arms of `emitter/flows.rs::emit_child_fanout` /
//! `emitter/implicit_children.rs::emit_fanout`.
//!
//! Parallel `distribute` runs two passes: pass one spawns EVERY child
//! (accumulating handles reversed), pass two awaits each handle **in item
//! order** (`list.reverse(handles)` first), then restores result order.
//! Strict aborts the whole step on the first spawn or await failure;
//! tolerant captures exactly one `Option` slot per input item (a spawn
//! failure still lets later spawns happen). `sequence` runs
//! `workflow.spawn_and_wait` one item at a time through the shared fold
//! closers.
//!
//! Both the single declared-child track and the implicit multi-step-child
//! track share these builders: only the spawn input differs (declared
//! parameters matched by name vs the wrapper-parameter `json.object`).

use crate::ast::{CallStmt, DeliveryVerb};
use crate::emitter::{GType, type_ref_to_g};

use super::super::func::MirFn;
use super::super::ids::{Span, Var};
use super::super::ops::{Block, JsonVal, Stmt, Tail, Test, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::call_rt;
use super::child_call::{child_config_refusal, child_input_json, spawn_wait_args, to_json_ref};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope};
use super::fanout::{
    Fanout, FoldResult, call_args_contain_index, capture_values, closure_frame, fanout_fn,
    finish_fold_body, fold_call_site, gathered_desc, try_bind, wrapper_free_names,
};
use super::flow::FlowEnv;
use super::forks::branch_free_names;
use super::slots::Slots;

/// One child track's lowering surface.
struct ChildTrack<'a> {
    env: FlowEnv<'a>,
    fanout: &'a Fanout<'a>,
    kind: SpawnKind<'a>,
    free: &'a [String],
    scope: &'a Scope,
}

/// How one per-item child spawn builds its input and names its target.
enum SpawnKind<'a> {
    /// A single declared-child call: parameters matched by name.
    Declared {
        call: &'a crate::ast::Call,
        returns: GType,
    },
    /// The implicit multi-step child: a `json.object` over wrapper params.
    Implicit,
}

/// Fan out a single declared-child track.
pub(super) fn lower_declared_child_fanout(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    fanout: &Fanout<'_>,
    call: &CallStmt,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    if call.config.is_some() {
        return Err(child_config_refusal(call.span));
    }
    let returns = ctx
        .emitter
        .children
        .get(call.call.name.as_str())
        .map(|child| type_ref_to_g(&child.returns))
        .ok_or_else(|| LowerError::new(call.call.name_span, "child declaration disappeared"))?;
    let free = branch_free_names(&call.call, &fanout.region.var, scope);
    let kind = SpawnKind::Declared {
        call: &call.call,
        returns,
    };
    let track = ChildTrack {
        env,
        fanout,
        kind,
        free: &free,
        scope,
    };
    match fanout.region.verb {
        DeliveryVerb::Sequence => {
            let (ordinal, self_ref) = slots.forks.take()?;
            let saved = ctx.swap_var_counter(0);
            let body = build_sequence_fold(ctx, &track, ordinal);
            ctx.swap_var_counter(saved);
            slots.forks.finish(ordinal, MirFn::Flow(body?));
            fold_call_site(
                ctx,
                fanout,
                self_ref,
                &free,
                scope,
                stmts,
                fanout.region.tolerant,
            )
        }
        DeliveryVerb::Distribute => {
            if call_args_contain_index(call) {
                return Err(LowerError::unsupported(
                    "indexing inside a parallel per-item track",
                    call.span,
                ));
            }
            spawn_all_await_ordered(ctx, &track, stmts, slots)
        }
    }
}

/// Fan out a multi-step `distribute` track: one synthesized child workflow
/// per item, spawn-all then await-each in item order.
pub(super) fn lower_implicit_child_fanout(
    ctx: &mut Ctx<'_>,
    env: FlowEnv<'_>,
    fanout: &Fanout<'_>,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let free = wrapper_free_names(fanout);
    let track = ChildTrack {
        env,
        fanout,
        kind: SpawnKind::Implicit,
        free: &free,
        scope,
    };
    spawn_all_await_ordered(ctx, &track, stmts, slots)
}

/// The parallel two-pass call site: spawn fold, `list.reverse`, await fold in
/// item order, `list.reverse`.
fn spawn_all_await_ordered(
    ctx: &mut Ctx<'_>,
    track: &ChildTrack<'_>,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<Var, LowerError> {
    let fanout = track.fanout;
    let tolerant = fanout.region.tolerant;
    let (spawn_ordinal, spawn_ref) = slots.forks.take()?;
    let saved = ctx.swap_var_counter(0);
    let spawn_fn = build_spawn_fold(ctx, track, spawn_ordinal);
    ctx.swap_var_counter(saved);
    slots.forks.finish(spawn_ordinal, MirFn::Flow(spawn_fn?));

    let (await_ordinal, await_ref) = slots.forks.take()?;
    let saved = ctx.swap_var_counter(0);
    let await_fn = build_await_fold(ctx, fanout, await_ordinal);
    ctx.swap_var_counter(saved);
    slots.forks.finish(await_ordinal, MirFn::Flow(await_fn?));

    // Pass one must finish before pass two begins: every child has a
    // distinct spawned handle before any await.
    let span = Span::from_source(fanout.span);
    let captures = capture_values(fanout.span, track.scope, track.free)?;
    let spawn_closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: spawn_closure,
        lifted: spawn_ref,
        captures,
        span,
    });
    let fold = if tolerant {
        RuntimeFn::LFold
    } else {
        RuntimeFn::LTryFold
    };
    let handles_folded = call_rt(
        ctx,
        fold,
        vec![fanout.items.clone(), Value::Nil, Value::Var(spawn_closure)],
        stmts,
        fanout.span,
    );
    let handles_reversed = if tolerant {
        handles_folded
    } else {
        try_bind(ctx, handles_folded, stmts, fanout.span)
    };
    // Item order: reverse the accumulated (reversed) handle list first.
    let handles = call_rt(
        ctx,
        RuntimeFn::LReverse,
        vec![Value::Var(handles_reversed)],
        stmts,
        fanout.span,
    );
    let await_closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: await_closure,
        lifted: await_ref,
        captures: Vec::new(),
        span,
    });
    let results_folded = call_rt(
        ctx,
        fold,
        vec![Value::Var(handles), Value::Nil, Value::Var(await_closure)],
        stmts,
        fanout.span,
    );
    let results_reversed = if tolerant {
        results_folded
    } else {
        try_bind(ctx, results_folded, stmts, fanout.span)
    };
    Ok(call_rt(
        ctx,
        RuntimeFn::LReverse,
        vec![Value::Var(results_reversed)],
        stmts,
        fanout.span,
    ))
}

/// One per-item spawn: the input object, then the six-argument string-name
/// `workflow.spawn` tail.
fn spawn_result(
    ctx: &mut Ctx<'_>,
    track: &ChildTrack<'_>,
    fn_scope: &Scope,
    stmts: &mut Vec<Stmt>,
    spawn_fn: RuntimeFn,
) -> Result<Var, LowerError> {
    let env = track.env;
    let fanout = track.fanout;
    let (name, returns, input) = match &track.kind {
        SpawnKind::Declared { call, returns } => {
            let input = child_input_json(ctx, env.plan, call, fn_scope, stmts)?;
            (call.name.clone(), returns.clone(), input)
        }
        SpawnKind::Implicit => {
            let mut pairs = Vec::new();
            for name in &fanout.nested.wrapper_params {
                let binding = fn_scope.get(name).ok_or_else(|| {
                    LowerError::new(
                        fanout.span,
                        format!("implicit child parameter `{name}` is not in scope"),
                    )
                })?;
                pairs.push((
                    name.clone(),
                    JsonVal::Encoded {
                        value: Value::Var(binding.var),
                        via: to_json_ref(ctx, env.plan, &binding.ty)?,
                    },
                ));
            }
            let input = ctx.fresh_var();
            stmts.push(Stmt::JsonObj {
                dst: input,
                pairs,
                span: Span::from_source(fanout.span),
            });
            (
                fanout.region.child_name.clone(),
                fanout.item_ty.clone(),
                input,
            )
        }
    };
    let args = spawn_wait_args(ctx, env.plan, &name, fanout.span, &returns, input, stmts)?;
    Ok(call_rt(ctx, spawn_fn, args, stmts, fanout.span))
}

/// Pass one's fold body: spawn one child, capture its handle (strict try /
/// tolerant `Option` slot — a spawn failure still lets later spawns happen).
fn build_spawn_fold(
    ctx: &mut Ctx<'_>,
    track: &ChildTrack<'_>,
    ordinal: usize,
) -> Result<super::super::func::FlowFn, LowerError> {
    let fanout = track.fanout;
    let tolerant = fanout.region.tolerant;
    let handle_desc = child_handle_desc(ctx, fanout);
    let acc_desc = if tolerant {
        TyDesc::List(Box::new(TyDesc::Option(Box::new(handle_desc))))
    } else {
        TyDesc::List(Box::new(handle_desc))
    };
    let acc = ctx.fresh_var();
    let item = ctx.fresh_var();
    let elem_desc = ctx.tydesc(&fanout.elem_ty);
    let (params, param_tys, mut fn_scope) = closure_frame(
        ctx,
        fanout.span,
        track.scope,
        track.free,
        &[(acc, acc_desc.clone()), (item, elem_desc)],
    )?;
    fn_scope.insert(
        fanout.region.var.clone(),
        Binding {
            var: item,
            ty: fanout.elem_ty.clone(),
        },
    );
    let mut stmts = Vec::new();
    let spawned = spawn_result(ctx, track, &fn_scope, &mut stmts, RuntimeFn::WfSpawn)?;
    finish_fold_body(
        ctx,
        fanout,
        ordinal,
        (params, param_tys),
        acc_desc,
        stmts,
        FoldResult {
            result: spawned,
            acc,
            tolerant,
            map_error: Some(RuntimeFn::MapSpawnError),
        },
    )
}

/// Pass two's fold body: await one handle in item order (strict try /
/// tolerant nested `Option` capture — a `None` handle slot stays `None`).
fn build_await_fold(
    ctx: &mut Ctx<'_>,
    fanout: &Fanout<'_>,
    ordinal: usize,
) -> Result<super::super::func::FlowFn, LowerError> {
    let tolerant = fanout.region.tolerant;
    let handle_desc = child_handle_desc(ctx, fanout);
    let acc_desc = gathered_desc(ctx, fanout);
    let span = Span::from_source(fanout.span);
    let acc = ctx.fresh_var();
    let slot = ctx.fresh_var();
    let slot_desc = if tolerant {
        TyDesc::Option(Box::new(handle_desc))
    } else {
        handle_desc
    };
    let params = vec![acc, slot];
    let param_tys = vec![acc_desc.clone(), slot_desc];
    if tolerant {
        let some = ctx.atom("some");
        let then_block = tolerant_await_arm(ctx, fanout, slot, acc);
        let else_block = none_slot_block(ctx, acc, span);
        return fanout_fn(
            &fanout.region.open_name,
            fanout.span,
            ordinal,
            (params, param_tys),
            acc_desc,
            Block {
                stmts: Vec::new(),
                tail: Tail::If {
                    test: Test::IsTagged {
                        value: Value::Var(slot),
                        tag: some,
                        arity: 2,
                    },
                    then_block: Box::new(then_block),
                    else_block: Box::new(else_block),
                    span,
                },
            },
        );
    }
    let mut stmts = Vec::new();
    let awaited = call_rt(
        ctx,
        RuntimeFn::ChildAwait,
        vec![Value::Var(slot)],
        &mut stmts,
        fanout.span,
    );
    finish_fold_body(
        ctx,
        fanout,
        ordinal,
        (params, param_tys),
        acc_desc,
        stmts,
        FoldResult {
            result: awaited,
            acc,
            tolerant: false,
            map_error: Some(RuntimeFn::MapChildError),
        },
    )
}

/// The tolerant await's live arm: unwrap the handle, await it, and capture
/// the per-item slot (`Ok -> Some(item)`, `Error -> None`).
fn tolerant_await_arm(ctx: &mut Ctx<'_>, fanout: &Fanout<'_>, slot: Var, acc: Var) -> Block {
    let span = Span::from_source(fanout.span);
    let some = ctx.atom("some");
    let ok = ctx.atom("ok");
    let live = ctx.fresh_var();
    let awaited = ctx.fresh_var();
    let item = ctx.fresh_var();
    let wrapped = ctx.fresh_var();
    let kept = ctx.fresh_var();
    let ok_block = Block {
        stmts: vec![
            Stmt::FieldGet {
                dst: item,
                base: Value::Var(awaited),
                index: 1,
                span,
            },
            Stmt::RecordNew {
                dst: wrapped,
                tag: some,
                args: vec![Value::Var(item)],
                span,
            },
            Stmt::ListPrepend {
                dst: kept,
                head: Value::Var(wrapped),
                tail: Value::Var(acc),
                span,
            },
        ],
        tail: Tail::Return(Value::Var(kept)),
    };
    let error_block = none_slot_block(ctx, acc, span);
    Block {
        stmts: vec![
            Stmt::FieldGet {
                dst: live,
                base: Value::Var(slot),
                index: 1,
                span,
            },
            Stmt::CallRt {
                dst: Some(awaited),
                callee: RuntimeFn::ChildAwait,
                args: vec![Value::Var(live)],
                live_after: super::super::ops::LiveAfter::default(),
                span,
            },
        ],
        tail: Tail::If {
            test: Test::IsTagged {
                value: Value::Var(awaited),
                tag: ok,
                arity: 2,
            },
            then_block: Box::new(ok_block),
            else_block: Box::new(error_block),
            span,
        },
    }
}

/// A `None` slot prepended to the accumulator and returned.
fn none_slot_block(ctx: &mut Ctx<'_>, acc: Var, span: Span) -> Block {
    let none = ctx.atom("none");
    let missing = ctx.fresh_var();
    Block {
        stmts: vec![Stmt::ListPrepend {
            dst: missing,
            head: Value::Atom(none),
            tail: Value::Var(acc),
            span,
        }],
        tail: Tail::Return(Value::Var(missing)),
    }
}

/// A `sequence` child fold body: `workflow.spawn_and_wait` per item.
fn build_sequence_fold(
    ctx: &mut Ctx<'_>,
    track: &ChildTrack<'_>,
    ordinal: usize,
) -> Result<super::super::func::FlowFn, LowerError> {
    let fanout = track.fanout;
    let acc = ctx.fresh_var();
    let item = ctx.fresh_var();
    let acc_desc = gathered_desc(ctx, fanout);
    let elem_desc = ctx.tydesc(&fanout.elem_ty);
    let (params, param_tys, mut fn_scope) = closure_frame(
        ctx,
        fanout.span,
        track.scope,
        track.free,
        &[(acc, acc_desc.clone()), (item, elem_desc)],
    )?;
    fn_scope.insert(
        fanout.region.var.clone(),
        Binding {
            var: item,
            ty: fanout.elem_ty.clone(),
        },
    );
    let mut stmts = Vec::new();
    let waited = spawn_result(ctx, track, &fn_scope, &mut stmts, RuntimeFn::WfSpawnAndWait)?;
    finish_fold_body(
        ctx,
        fanout,
        ordinal,
        (params, param_tys),
        acc_desc,
        stmts,
        FoldResult {
            result: waited,
            acc,
            tolerant: fanout.region.tolerant,
            map_error: Some(RuntimeFn::MapChildError),
        },
    )
}

fn child_handle_desc(ctx: &Ctx<'_>, fanout: &Fanout<'_>) -> TyDesc {
    TyDesc::ChildHandle(
        Box::new(ctx.tydesc(&fanout.item_ty)),
        Box::new(TyDesc::AwlError),
    )
}
