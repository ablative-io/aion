//! Union `_decoder` bodies (BC-2b-5, split from `codec_decode` for the
//! 500-line law): the outcome-string dispatch, the per-arm payload
//! continuations, and the OP-built zero default (S5) unknown outcomes fail
//! with — the reference `emitter/codecs.rs::union_codec` recipe exactly.

use super::super::func::{FlowFn, MirFn};
use super::super::ids::FnRef;
use super::super::ops::{Block, CmpOp, LiveAfter, Stmt, Tail, Test, Value};
use super::super::runtime::RuntimeFn;
use super::super::shapes::{TypeShape, UnionArm, WireDesc};
use super::super::tydesc::{Leaf, TyDesc};
use super::build::{CodecType, FnPlan};
use super::codec::{Stamped, decoder_value, desc_tydesc, lifted_origin, zero_span};
use super::codec_decode::{decoder_flow, missing_slot};
use super::ctx::Ctx;
use super::driver::LowerError;

/// Union `_decoder`: `decode.field("outcome", decode.string)` into a lifted
/// case; each arm decodes `"payload"` with its own codec through an arm
/// continuation; unknown outcomes fail with the OP-built first-arm zero.
pub(super) fn union(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    types: &[TypeShape],
    codec_type: &CodecType,
    shape: &TypeShape,
    lifted_refs: &[FnRef],
) -> Result<Stamped, LowerError> {
    let TypeShape::Union { name, arms } = shape else {
        return Err(LowerError::new(
            ctx.emitter.document.span,
            format!("codec `{}` is not a union shape", codec_type.stem),
        ));
    };
    let name = name.clone();
    let arms = arms.clone();
    let first = arms.first().cloned().ok_or_else(|| {
        LowerError::new(
            ctx.emitter.document.span,
            "the outcome union has no success arms",
        )
    })?;
    let outcome_cont_ref = lifted_refs
        .first()
        .copied()
        .ok_or_else(|| missing_slot(ctx, codec_type))?;
    let arm_refs = lifted_refs.get(1..).unwrap_or_default();
    if arm_refs.len() != arms.len() {
        return Err(missing_slot(ctx, codec_type));
    }
    let (_, _, decoder_ref) = plan.codecs[&codec_type.stem];

    let main = union_main(ctx, codec_type, outcome_cont_ref);
    let outcome_cont = union_outcome_cont(
        ctx,
        plan,
        types,
        codec_type,
        &name,
        &first,
        &arms,
        arm_refs,
        decoder_ref,
    )?;
    let mut lifted = vec![outcome_cont];
    union_arm_conts(ctx, codec_type, &arms, decoder_ref, &mut lifted);
    Ok(Stamped { main, lifted })
}

/// Union `_decoder` top level: `decode.field("outcome", string_decoder(), cont)`.
fn union_main(ctx: &mut Ctx<'_>, codec_type: &CodecType, outcome_cont_ref: FnRef) -> MirFn {
    ctx.reset_vars();
    let outcome_lit = ctx.binary("outcome");
    let string_decoder = ctx.fresh_var();
    let cont = ctx.fresh_var();
    decoder_flow(
        codec_type,
        Block {
            stmts: vec![
                Stmt::CallRt {
                    dst: Some(string_decoder),
                    callee: RuntimeFn::LeafDecoder(Leaf::Str),
                    args: Vec::new(),
                    live_after: LiveAfter::default(),
                    span: zero_span(),
                },
                Stmt::MakeClosure {
                    dst: cont,
                    lifted: outcome_cont_ref,
                    captures: Vec::new(),
                    span: zero_span(),
                },
            ],
            tail: Tail::TailRt {
                callee: RuntimeFn::DField,
                args: vec![
                    Value::Lit(outcome_lit),
                    Value::Var(string_decoder),
                    Value::Var(cont),
                ],
            },
        },
    )
}

/// The outcome continuation: match every arm's outcome name; unknown
/// outcomes fail with the typed first-arm default (S5).
#[expect(
    clippy::too_many_arguments,
    reason = "one stamping site; the values are the continuation's full identity"
)]
fn union_outcome_cont(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    types: &[TypeShape],
    codec_type: &CodecType,
    name: &str,
    first: &UnionArm,
    arms: &[UnionArm],
    arm_refs: &[FnRef],
    decoder_ref: FnRef,
) -> Result<MirFn, LowerError> {
    ctx.reset_vars();
    let value = ctx.fresh_var();
    let mut fail_stmts = Vec::new();
    let zero = zero_value(ctx, types, &first.payload, &mut fail_stmts, &mut Vec::new())?;
    let fallback = ctx.fresh_var();
    fail_stmts.push(Stmt::RecordNew {
        dst: fallback,
        tag: first.ctor,
        args: vec![zero],
        span: zero_span(),
    });
    let union_lit = ctx.binary(name);
    let mut tail_block = Block {
        stmts: fail_stmts,
        tail: Tail::TailRt {
            callee: RuntimeFn::DFailure,
            args: vec![Value::Var(fallback), Value::Lit(union_lit)],
        },
    };
    for (position, arm) in arms.iter().enumerate().rev() {
        let outcome_name_lit = ctx.binary(&arm.outcome);
        let mut arm_stmts = Vec::new();
        let payload_decoder = decoder_value(ctx, plan, &arm.payload, &mut arm_stmts)?;
        let arm_cont = ctx.fresh_var();
        arm_stmts.push(Stmt::MakeClosure {
            dst: arm_cont,
            lifted: arm_refs[position],
            captures: Vec::new(),
            span: zero_span(),
        });
        let payload_lit = ctx.binary("payload");
        tail_block = Block {
            stmts: Vec::new(),
            tail: Tail::If {
                test: Test::Cmp {
                    op: CmpOp::Eq,
                    lhs: Value::Var(value),
                    rhs: Value::Lit(outcome_name_lit),
                },
                then_block: Box::new(Block {
                    stmts: arm_stmts,
                    tail: Tail::TailRt {
                        callee: RuntimeFn::DField,
                        args: vec![
                            Value::Lit(payload_lit),
                            Value::Var(payload_decoder),
                            Value::Var(arm_cont),
                        ],
                    },
                }),
                else_block: Box::new(tail_block),
                span: zero_span(),
            },
        };
    }
    Ok(MirFn::Flow(FlowFn {
        origin: lifted_origin(decoder_ref, 0),
        name: format!("{}_decoder$outcome", codec_type.stem),
        params: vec![value],
        param_tys: vec![TyDesc::String],
        ret_ty: TyDesc::Decoder(Box::new(codec_type.tydesc.clone())),
        body: tail_block,
        span: zero_span(),
        degraded_parallel: false,
    }))
}

/// Arm continuations: wrap the decoded payload in the arm constructor.
fn union_arm_conts(
    ctx: &mut Ctx<'_>,
    codec_type: &CodecType,
    arms: &[UnionArm],
    decoder_ref: FnRef,
    lifted: &mut Vec<MirFn>,
) {
    for (position, arm) in arms.iter().enumerate() {
        ctx.reset_vars();
        let payload = ctx.fresh_var();
        let wrapped = ctx.fresh_var();
        lifted.push(MirFn::Flow(FlowFn {
            origin: lifted_origin(decoder_ref, u32::try_from(position + 1).unwrap_or(u32::MAX)),
            name: format!("{}_decoder$arm{position}", codec_type.stem),
            params: vec![payload],
            param_tys: vec![desc_tydesc(ctx, &arm.payload)],
            ret_ty: TyDesc::Decoder(Box::new(codec_type.tydesc.clone())),
            body: Block {
                stmts: vec![Stmt::RecordNew {
                    dst: wrapped,
                    tag: arm.ctor,
                    args: vec![Value::Var(payload)],
                    span: zero_span(),
                }],
                tail: Tail::TailRt {
                    callee: RuntimeFn::DSuccess,
                    args: vec![Value::Var(wrapped)],
                },
            },
            span: zero_span(),
            degraded_parallel: false,
        }));
    }
}

/// An OP-built zero value of a wire shape (S5): the `decode.failure` typed
/// default. Required recursive cycles are refused with the reference
/// emitter's exact diagnostic class (`emitter/types.rs::zero_expr`), pinned
/// two-sided by `mir/codec_tests.rs`. Deliberately `LowerError::Message` (a
/// hard failure, not an `Unsupported`/`Planning` refusal): the BC-3 oracle
/// must fail LOUDLY if a corpus fixture ever reaches this, matching the
/// reference emitter's own hard emit-time error.
fn zero_value(
    ctx: &mut Ctx<'_>,
    types: &[TypeShape],
    desc: &WireDesc,
    stmts: &mut Vec<Stmt>,
    visiting: &mut Vec<String>,
) -> Result<Value, LowerError> {
    Ok(match desc {
        WireDesc::Bool => Value::Atom(ctx.atom("false")),
        WireDesc::Int => Value::Int(0),
        WireDesc::Float => Value::Lit(ctx.push_float("0.0")),
        WireDesc::Str => Value::Lit(ctx.binary("")),
        WireDesc::Nil => Value::Atom(ctx.atom("nil")),
        WireDesc::List(_) => Value::Nil,
        WireDesc::Nullable(_) => Value::Atom(ctx.atom("none")),
        WireDesc::Ref(name) => {
            if visiting.iter().any(|seen| seen == name) {
                return Err(LowerError::new(
                    ctx.emitter.document.span,
                    format!(
                        "type `{name}` recurses through required fields, so no default \
                         value exists for the generated decoder"
                    ),
                ));
            }
            let shape = types
                .iter()
                .find(|shape| shape.name() == name)
                .ok_or_else(|| {
                    LowerError::new(
                        ctx.emitter.document.span,
                        format!("reference to undeclared type `{name}`"),
                    )
                })?
                .clone();
            match shape {
                TypeShape::Record { tag, fields, .. } => {
                    visiting.push(name.clone());
                    let mut args = Vec::with_capacity(fields.len());
                    for field in &fields {
                        args.push(zero_value(ctx, types, &field.desc, stmts, visiting)?);
                    }
                    visiting.pop();
                    let record = ctx.fresh_var();
                    stmts.push(Stmt::RecordNew {
                        dst: record,
                        tag,
                        args,
                        span: zero_span(),
                    });
                    Value::Var(record)
                }
                TypeShape::Enum { variants, .. } => {
                    let (ctor, _) = variants.first().cloned().ok_or_else(|| {
                        LowerError::new(
                            ctx.emitter.document.span,
                            format!("enum `{name}` has no variants"),
                        )
                    })?;
                    Value::Atom(ctor)
                }
                TypeShape::Union { .. } => {
                    return Err(LowerError::new(
                        ctx.emitter.document.span,
                        format!("no default value exists for union `{name}`"),
                    ));
                }
            }
        }
    })
}
