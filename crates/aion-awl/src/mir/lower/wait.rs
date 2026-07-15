//! `wait` statement lowering — the MIR twin of the reference
//! `emitter/stmts.rs:269-315`.
//!
//! No-timeout form: `workflow.receive({snake}_signal()) |>
//! awl_error.map_receive_error`, `TryBind`, bind = the signal payload type.
//!
//! Timeout form: the reference wraps the receive in
//! `workflow.with_timeout(fn() { receive… }, duration)` and a 4-arm case —
//! `Ok(value) -> Ok(Some(value))`, `Error(error.TimedOutError(_)) ->
//! Ok(None)`, `Error(error.InnerError(inner)) -> Error(inner)`,
//! `Error(error.TimeoutEngineFailure(message)) ->
//! Error(awl_error.AwlTimerFailed(message))`; bind = `Option(payload)`. MIR
//! control flow is a tree per function with no reconvergence, but the case
//! reconverges on the ok/error shape — so the case lowers as a
//! module-local LIFTED function whose leaves are returns, and the host
//! reconverges with one `TryBind`. Each timeout wait consumes TWO reserved
//! wait slots (`{step}_wait_{k}_receive/0` then `{step}_wait_{k}_case/1`,
//! `count_wait_fns`), so modules without timeout waits keep byte-identical
//! `FnRef`s. All error variants of the SDK `TimeoutResultError` are plain
//! 2-tuples (`TimedOutError(TimeoutError)` / `InnerError(error)` /
//! `TimeoutEngineFailure(message)` — `aion/error.gleam:166-170`), so every
//! `IsTagged` test carries arity 2.

use crate::ast::{Statement, Step, WaitStmt};
use crate::emitter::{GType, snake, type_ref_to_g};

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{Span, Var};
use super::super::ops::{Block, LiveAfter, Stmt, Tail, Test, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::{call_rt, duration_ms};
use super::build::FnPlan;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope};
use super::slots::Slots;

/// The wait-lifted-function inventory a document's regions will consume, in
/// the exact traversal order lowering encounters them: statements in written
/// order with the `lower_step` early-stop, descending into loop bodies
/// pre-order — the same discipline as `forks::count_fork_fns`. A timeout
/// wait takes two slots (receive fn + case fn); a plain wait takes none.
pub(super) fn count_wait_fns(statements: &[Statement]) -> u32 {
    let mut count = 0;
    for statement in statements {
        match statement {
            Statement::Wait(wait) if wait.timeout.is_some() => count += 2,
            Statement::Loop(looped) => count += count_wait_fns(&looped.body),
            Statement::Route(_) => break,
            Statement::Pipe(pipe) if matches!(pipe.end, crate::ast::PipeEnd::Route(_)) => break,
            _ => {}
        }
    }
    count
}

pub(super) fn lower_wait_stmt(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    step: &Step,
    wait: &WaitStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let signal = ctx
        .emitter
        .signals
        .get(wait.signal.as_str())
        .ok_or_else(|| {
            LowerError::new(
                wait.signal_span,
                format!("`{}` names no declared signal", wait.signal),
            )
        })?;
    let payload_ty = type_ref_to_g(&signal.ty);
    let signal_ref =
        *plan
            .signals
            .get(wait.signal.as_str())
            .ok_or_else(|| LowerError::Planning {
                message: format!("signal `{}` has no planned shell", wait.signal),
            })?;
    match &wait.timeout {
        None => {
            lower_plain_wait(ctx, wait, signal_ref, payload_ty, scope, stmts);
            Ok(())
        }
        Some(timeout) => {
            let build = TimeoutWait {
                step,
                wait,
                payload_ty: &payload_ty,
            };
            let (receive_ref, case_ref) = build_lifted_fns(ctx, &build, signal_ref, slots)?;
            let span = Span::from_source(wait.span);
            let closure = ctx.fresh_var();
            stmts.push(Stmt::MakeClosure {
                dst: closure,
                lifted: receive_ref,
                captures: Vec::new(),
                span,
            });
            let ms = duration_ms(timeout.magnitude, timeout.unit);
            let dur = call_rt(
                ctx,
                RuntimeFn::DurationMs,
                vec![Value::Int(ms)],
                stmts,
                wait.span,
            );
            // SDK arg order: operation, then deadline (`workflow.gleam`).
            let raw = call_rt(
                ctx,
                RuntimeFn::WfWithTimeout,
                vec![Value::Var(closure), Value::Var(dur)],
                stmts,
                wait.span,
            );
            let cased = ctx.fresh_var();
            stmts.push(Stmt::CallLocal {
                dst: Some(cased),
                callee: case_ref,
                args: vec![Value::Var(raw)],
                live_after: LiveAfter::default(),
                span,
            });
            let decision = ctx.fresh_var();
            stmts.push(Stmt::TryBind {
                dst: decision,
                result: cased,
                live_after: LiveAfter::default(),
                span,
            });
            scope.insert(
                wait.bind.name.clone(),
                Binding {
                    var: decision,
                    ty: GType::Option(Box::new(payload_ty.clone())),
                },
            );
            Ok(())
        }
    }
}

/// The no-timeout form: `workflow.receive({snake}_signal()) |>
/// map_receive_error` + `TryBind`, bind = the signal payload type.
fn lower_plain_wait(
    ctx: &mut Ctx<'_>,
    wait: &WaitStmt,
    signal_ref: super::super::ids::FnRef,
    payload_ty: GType,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) {
    let sig = ctx.fresh_var();
    stmts.push(Stmt::CallLocal {
        dst: Some(sig),
        callee: signal_ref,
        args: Vec::new(),
        live_after: LiveAfter::default(),
        span: Span::from_source(wait.signal_span),
    });
    let received = call_rt(
        ctx,
        RuntimeFn::WfReceive,
        vec![Value::Var(sig)],
        stmts,
        wait.span,
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapReceiveError,
        vec![Value::Var(received)],
        stmts,
        wait.span,
    );
    let bound = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: bound,
        result: mapped,
        live_after: LiveAfter::default(),
        span: Span::from_source(wait.span),
    });
    scope.insert(
        wait.bind.name.clone(),
        Binding {
            var: bound,
            ty: payload_ty,
        },
    );
}

struct TimeoutWait<'a> {
    step: &'a Step,
    wait: &'a WaitStmt,
    payload_ty: &'a GType,
}

/// Build the two lifted functions for one timeout wait, consuming the two
/// reserved slots in order (receive first, case second — the reservation
/// order `count_wait_fns` pins).
fn build_lifted_fns(
    ctx: &mut Ctx<'_>,
    build: &TimeoutWait<'_>,
    signal_ref: super::super::ids::FnRef,
    slots: &mut Slots,
) -> Result<(super::super::ids::FnRef, super::super::ids::FnRef), LowerError> {
    let (receive_ordinal, receive_ref) = slots.waits.take()?;
    let wait_index = receive_ordinal / 2;
    let saved = ctx.swap_var_counter(0);
    let receive_fn = build_receive_fn(ctx, build, signal_ref, receive_ordinal, wait_index);
    ctx.swap_var_counter(saved);
    slots.waits.finish(receive_ordinal, MirFn::Flow(receive_fn));

    let (case_ordinal, case_ref) = slots.waits.take()?;
    let saved = ctx.swap_var_counter(0);
    let case_fn = build_case_fn(ctx, build, case_ordinal, wait_index);
    ctx.swap_var_counter(saved);
    slots.waits.finish(case_ordinal, MirFn::Flow(case_fn));
    Ok((receive_ref, case_ref))
}

/// `{step}_wait_{k}_receive/0`: `CallLocal {sig}_signal()` →
/// `workflow.receive` → `Tail::TailRt map_receive_error`.
fn build_receive_fn(
    ctx: &mut Ctx<'_>,
    build: &TimeoutWait<'_>,
    signal_ref: super::super::ids::FnRef,
    ordinal: usize,
    wait_index: usize,
) -> FlowFn {
    let wait = build.wait;
    let span = Span::from_source(wait.span);
    let mut stmts = Vec::new();
    let sig = ctx.fresh_var();
    stmts.push(Stmt::CallLocal {
        dst: Some(sig),
        callee: signal_ref,
        args: Vec::new(),
        live_after: LiveAfter::default(),
        span: Span::from_source(wait.signal_span),
    });
    let received = call_rt(
        ctx,
        RuntimeFn::WfReceive,
        vec![Value::Var(sig)],
        &mut stmts,
        wait.span,
    );
    FlowFn {
        origin: FnOrigin::Wait {
            step: build.step.name.clone(),
            index: u32::try_from(ordinal).unwrap_or(u32::MAX),
        },
        name: format!("{}_wait_{}_receive", snake(&build.step.name), wait_index),
        params: Vec::new(),
        param_tys: Vec::new(),
        ret_ty: TyDesc::Result(
            Box::new(ctx.tydesc(build.payload_ty)),
            Box::new(TyDesc::AwlError),
        ),
        body: Block {
            stmts,
            tail: Tail::TailRt {
                callee: RuntimeFn::MapReceiveError,
                args: vec![Value::Var(received)],
            },
        },
        span,
        degraded_parallel: false,
    }
}

/// `{step}_wait_{k}_case/1`: the 4-arm timeout case as nested `Tail::If`
/// leaves, each returning — the host reconverges with one `TryBind`.
fn build_case_fn(
    ctx: &mut Ctx<'_>,
    build: &TimeoutWait<'_>,
    ordinal: usize,
    wait_index: usize,
) -> FlowFn {
    let span = Span::from_source(build.wait.span);
    let subject = ctx.fresh_var();
    let then_block = case_ok_arm(ctx, subject, span);
    let error_block = case_error_arm(ctx, subject, span);
    let ok = ctx.atom("ok");
    let body = Block {
        stmts: Vec::new(),
        tail: Tail::If {
            test: Test::IsTagged {
                value: Value::Var(subject),
                tag: ok,
                arity: 2,
            },
            then_block: Box::new(then_block),
            else_block: Box::new(error_block),
            span,
        },
    };
    FlowFn {
        origin: FnOrigin::Wait {
            step: build.step.name.clone(),
            index: u32::try_from(ordinal).unwrap_or(u32::MAX),
        },
        name: format!("{}_wait_{}_case", snake(&build.step.name), wait_index),
        params: vec![subject],
        // The error side is the SDK `TimeoutResultError(AwlError)` nominal;
        // it projects as the SDK module's custom type.
        param_tys: vec![TyDesc::Result(
            Box::new(ctx.tydesc(build.payload_ty)),
            Box::new(TyDesc::Custom {
                module: "aion/error".to_owned(),
                name: "TimeoutResultError".to_owned(),
                params: vec![TyDesc::AwlError],
            }),
        )],
        ret_ty: TyDesc::Result(
            Box::new(TyDesc::Option(Box::new(ctx.tydesc(build.payload_ty)))),
            Box::new(TyDesc::AwlError),
        ),
        body,
        span,
        degraded_parallel: false,
    }
}

/// `Ok(value) -> Ok(Some(value))` — the completed-in-time arm.
fn case_ok_arm(ctx: &mut Ctx<'_>, subject: Var, span: Span) -> Block {
    let ok = ctx.atom("ok");
    let some = ctx.atom("some");
    let payload = ctx.fresh_var();
    let wrapped = ctx.fresh_var();
    let ok_some = ctx.fresh_var();
    Block {
        stmts: vec![
            Stmt::FieldGet {
                dst: payload,
                base: Value::Var(subject),
                index: 1,
                span,
            },
            Stmt::RecordNew {
                dst: wrapped,
                tag: some,
                args: vec![Value::Var(payload)],
                span,
            },
            Stmt::RecordNew {
                dst: ok_some,
                tag: ok,
                args: vec![Value::Var(wrapped)],
                span,
            },
        ],
        tail: Tail::Return(Value::Var(ok_some)),
    }
}

/// The error side of the case: `Error(error.TimedOutError(_)) -> Ok(None)`,
/// `Error(error.InnerError(inner)) -> Error(inner)`, and
/// `Error(error.TimeoutEngineFailure(message)) ->
/// Error(awl_error.AwlTimerFailed(message))`.
fn case_error_arm(ctx: &mut Ctx<'_>, subject: Var, span: Span) -> Block {
    let ok = ctx.atom("ok");
    let none = ctx.atom("none");
    let error = ctx.atom("error");
    let timed_out = ctx.atom("timed_out_error");
    let inner_tag = ctx.atom("inner_error");
    let timer_failed = ctx.atom("awl_timer_failed");

    // Error(error.TimedOutError(_)) -> Ok(None)
    let ok_none = ctx.fresh_var();
    let timed_out_block = Block {
        stmts: vec![Stmt::RecordNew {
            dst: ok_none,
            tag: ok,
            args: vec![Value::Atom(none)],
            span,
        }],
        tail: Tail::Return(Value::Var(ok_none)),
    };

    let inner_payload = ctx.fresh_var();
    let inner_error = ctx.fresh_var();
    let carried = ctx.fresh_var();
    let message = ctx.fresh_var();
    let failed = ctx.fresh_var();
    let failed_error = ctx.fresh_var();
    let engine_block = Block {
        stmts: vec![
            Stmt::FieldGet {
                dst: message,
                base: Value::Var(carried),
                index: 1,
                span,
            },
            Stmt::RecordNew {
                dst: failed,
                tag: timer_failed,
                args: vec![Value::Var(message)],
                span,
            },
            Stmt::RecordNew {
                dst: failed_error,
                tag: error,
                args: vec![Value::Var(failed)],
                span,
            },
        ],
        tail: Tail::Return(Value::Var(failed_error)),
    };
    let inner_block = Block {
        stmts: vec![
            Stmt::FieldGet {
                dst: inner_payload,
                base: Value::Var(carried),
                index: 1,
                span,
            },
            Stmt::RecordNew {
                dst: inner_error,
                tag: error,
                args: vec![Value::Var(inner_payload)],
                span,
            },
        ],
        tail: Tail::Return(Value::Var(inner_error)),
    };
    Block {
        stmts: vec![Stmt::FieldGet {
            dst: carried,
            base: Value::Var(subject),
            index: 1,
            span,
        }],
        tail: Tail::If {
            test: Test::IsTagged {
                value: Value::Var(carried),
                tag: timed_out,
                arity: 2,
            },
            then_block: Box::new(timed_out_block),
            else_block: Box::new(Block {
                stmts: Vec::new(),
                tail: Tail::If {
                    test: Test::IsTagged {
                        value: Value::Var(carried),
                        tag: inner_tag,
                        arity: 2,
                    },
                    then_block: Box::new(inner_block),
                    else_block: Box::new(engine_block),
                    span,
                },
            }),
            span,
        },
    }
}
