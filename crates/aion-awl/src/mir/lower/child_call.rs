//! Shared child-spawn emission: the string-name spawn argument tuple
//! (witness, codecs, name literal) used by collection child forks, pipe
//! child stages, and the statement-level child forms; the shared JSON-input
//! builder (`child_input_json`); the awaited child call statement
//! (`workflow.spawn_and_wait |> map_child_error`, the reference
//! `emitter/stmts.rs:214-239`); and the fire-and-forget `spawn` statement
//! (`workflow.spawn |> map_spawn_error`, `emitter/stmts.rs:242-267`).

use crate::ast::{Call, CallStmt, SpawnStmt};
use crate::emitter::{GType, type_ref_to_g};

use super::super::func::CodecRef;
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{JsonVal, LiveAfter, Stmt, ToJsonRef, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::Leaf;
use super::activity::call_rt;
use super::build::{FnPlan, child_output_codec_ref_for, codec_ref_for};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_arg_for, wrap_optional_value};

/// The six-argument tail of a string-name child spawn (`spawn`/
/// `spawn_and_wait`): witness closure, input/output/error codecs, and the
/// child-name literal, over an already-built JSON input object.
pub(super) fn spawn_wait_args(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    name: &str,
    span: crate::Span,
    returns: &GType,
    input: Var,
    stmts: &mut Vec<Stmt>,
) -> Result<Vec<Value>, LowerError> {
    let witness_ref = plan.child_witness.ok_or_else(|| LowerError::Planning {
        message: "child spawn has no planned witness".to_owned(),
    })?;
    let witness = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: witness,
        lifted: witness_ref,
        captures: Vec::new(),
        span: Span::from_source(span),
    });
    let input_codec = call_rt(ctx, RuntimeFn::JsonValueCodec, Vec::new(), stmts, span);
    let output_codec_ref = child_output_codec_ref_for(ctx, plan, returns)?;
    let output_codec = codec_value(ctx, &output_codec_ref, stmts, span);
    let error_codec = call_rt(ctx, RuntimeFn::ErrCodec, Vec::new(), stmts, span);
    let name_lit = ctx.binary(name);
    Ok(vec![
        Value::Lit(name_lit),
        Value::Var(witness),
        Value::Var(input),
        Value::Var(input_codec),
        Value::Var(output_codec),
        Value::Var(error_codec),
    ])
}

/// Build the JSON input object for a string-name child spawn: declared
/// parameters matched by name in declared order, each argument lowered for
/// its slot and encoded through its wire type's `_to_json`, folded into one
/// `json.object` (the reference `child_spawn_args` input,
/// `emitter/stmts.rs:160-184`). Shared by collection child forks and the
/// statement-level child call/spawn forms.
pub(super) fn child_input_json(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    call: &Call,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<Var, LowerError> {
    let child = ctx
        .emitter
        .children
        .get(call.name.as_str())
        .ok_or_else(|| LowerError::new(call.name_span, "child declaration disappeared"))?;
    let params = child.params.clone();
    let mut pairs = Vec::new();
    for param in &params {
        let arg = call
            .args
            .iter()
            .find(|arg| arg.name == param.name)
            .ok_or_else(|| {
                LowerError::new(call.span, format!("call misses argument `{}`", param.name))
            })?;
        let ty = type_ref_to_g(&param.ty);
        let value = lower_arg_for(ctx, &arg.value, &ty, scope, stmts)?;
        pairs.push((
            param.name.clone(),
            JsonVal::Encoded {
                value,
                via: to_json_ref(ctx, plan, &ty)?,
            },
        ));
    }
    let input = ctx.fresh_var();
    stmts.push(Stmt::JsonObj {
        dst: input,
        pairs,
        span: Span::from_source(call.span),
    });
    Ok(input)
}

/// The reference's exact child-config refusal class: `node`/`timeout` cannot
/// pin a child workflow call (`emitter/stmts.rs:224-229`). Shared with the
/// collection-fork child branch gate (`forks::collection_branch`).
pub(super) fn child_config_refusal(span: crate::Span) -> LowerError {
    LowerError::new(
        span,
        "`node`/`timeout` cannot pin a child workflow call — the engine routes children, \
         not a queue",
    )
}

/// Lower an awaited child call statement: `workflow.spawn_and_wait<args> |>
/// awl_error.map_child_error`, bind = the child's declared return type
/// (`emitter/stmts.rs:214-239`).
pub(super) fn lower_child_call_stmt(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    call_stmt: &CallStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(), LowerError> {
    if call_stmt.config.is_some() {
        return Err(child_config_refusal(call_stmt.span));
    }
    let call = &call_stmt.call;
    let returns = ctx
        .emitter
        .children
        .get(call.name.as_str())
        .map(|child| type_ref_to_g(&child.returns))
        .ok_or_else(|| LowerError::new(call.name_span, "child declaration disappeared"))?;
    let input = child_input_json(ctx, plan, call, scope, stmts)?;
    let args = spawn_wait_args(
        ctx,
        plan,
        &call.name,
        call.name_span,
        &returns,
        input,
        stmts,
    )?;
    let waited = call_rt(ctx, RuntimeFn::WfSpawnAndWait, args, stmts, call.name_span);
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapChildError,
        vec![Value::Var(waited)],
        stmts,
        call.name_span,
    );
    let bound = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: bound,
        result: mapped,
        live_after: LiveAfter::default(),
        span: Span::from_source(call.name_span),
    });
    if let Some(bind) = &call_stmt.bind {
        scope.insert(
            bind.name.clone(),
            Binding {
                var: bound,
                ty: returns,
            },
        );
    }
    Ok(())
}

/// Lower a fire-and-forget `spawn` statement: `workflow.spawn<args> |>
/// awl_error.map_spawn_error`, result discarded through the try
/// (`emitter/stmts.rs:242-267`). A binding parses but is refused with the
/// reference's message.
pub(super) fn lower_spawn_stmt(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    spawn: &SpawnStmt,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(), LowerError> {
    if let Some(bind) = &spawn.bind {
        return Err(LowerError::new(
            bind.span,
            "`spawn` is fire-and-forget: binding its result is a check error",
        ));
    }
    let call = &spawn.call;
    let returns = ctx
        .emitter
        .children
        .get(call.name.as_str())
        .map(|child| type_ref_to_g(&child.returns))
        .ok_or_else(|| {
            LowerError::new(
                call.name_span,
                format!("`{}` names no declared child workflow", call.name),
            )
        })?;
    let input = child_input_json(ctx, plan, call, scope, stmts)?;
    let args = spawn_wait_args(
        ctx,
        plan,
        &call.name,
        call.name_span,
        &returns,
        input,
        stmts,
    )?;
    let spawned = call_rt(ctx, RuntimeFn::WfSpawn, args, stmts, call.name_span);
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapSpawnError,
        vec![Value::Var(spawned)],
        stmts,
        call.name_span,
    );
    let discard = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: discard,
        result: mapped,
        live_after: LiveAfter::default(),
        span: Span::from_source(call.name_span),
    });
    Ok(())
}

/// One single-argument child stage of a pipe chain: the piped value threads in
/// as the child's one declared parameter (`emitter/pipes.rs` child arm, 1:1).
pub(super) fn pipe_child_stage(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    name: &str,
    span: crate::Span,
    piped: (Value, GType),
    stmts: &mut Vec<Stmt>,
) -> Result<(Var, GType), LowerError> {
    let child = ctx
        .emitter
        .children
        .get(name)
        .ok_or_else(|| LowerError::new(span, "child declaration disappeared"))?;
    let returns = type_ref_to_g(&child.returns);
    let params = child.params.clone();
    let [param] = params.as_slice() else {
        // Checker-unreachable; mirrors the reference's one-argument gate.
        return Err(LowerError::unsupported("multi-arg child in pipe", span));
    };
    let expected = type_ref_to_g(&param.ty);
    let (value, value_ty) = piped;
    let wrapped = wrap_optional_value(ctx, value, &value_ty, &expected, stmts, span);
    let via = to_json_ref(ctx, plan, &expected)?;
    let input = ctx.fresh_var();
    stmts.push(Stmt::JsonObj {
        dst: input,
        pairs: vec![(
            param.name.clone(),
            JsonVal::Encoded {
                value: wrapped,
                via,
            },
        )],
        span: Span::from_source(span),
    });
    let args = spawn_wait_args(ctx, plan, name, span, &returns, input, stmts)?;
    let waited = call_rt(ctx, RuntimeFn::WfSpawnAndWait, args, stmts, span);
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapChildError,
        vec![Value::Var(waited)],
        stmts,
        span,
    );
    let bound = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: bound,
        result: mapped,
        live_after: LiveAfter::default(),
        span: Span::from_source(span),
    });
    Ok((bound, returns))
}

/// The `_to_json` reference for a wire type (one slot after `_codec`).
pub(super) fn to_json_ref(
    ctx: &Ctx<'_>,
    plan: &FnPlan,
    ty: &GType,
) -> Result<ToJsonRef, LowerError> {
    match codec_ref_for(ctx, plan, ty)? {
        CodecRef::SdkLeaf(leaf) => Ok(ToJsonRef::SdkLeaf(leaf)),
        CodecRef::Local(reference) => Ok(ToJsonRef::Local(FnRef(reference.0 + 1))),
        CodecRef::SdkNil => Ok(ToJsonRef::SdkLeaf(Leaf::Nil)),
    }
}

/// Materialize a codec reference as a runtime codec value.
pub(super) fn codec_value(
    ctx: &mut Ctx<'_>,
    codec: &CodecRef,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Var {
    match codec {
        CodecRef::Local(reference) => {
            let dst = ctx.fresh_var();
            stmts.push(Stmt::CallLocal {
                dst: Some(dst),
                callee: *reference,
                args: Vec::new(),
                live_after: LiveAfter::default(),
                span: Span::from_source(span),
            });
            dst
        }
        CodecRef::SdkNil => call_rt(ctx, RuntimeFn::NilCodec, Vec::new(), stmts, span),
        CodecRef::SdkLeaf(leaf) => {
            call_rt(ctx, RuntimeFn::LeafCodec(*leaf), Vec::new(), stmts, span)
        }
    }
}
