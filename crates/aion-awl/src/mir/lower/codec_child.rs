//! Parent-side child output codec stamping.
//!
//! AWL child modules honestly return their terminal outcome envelope. A parent
//! declaration's `-> T` contract selects the envelope's payload as `T`, so the
//! codec passed to child spawn strictly requires both `outcome` and `payload`,
//! ignores only the outcome string's value, and decodes the payload as `T`.
//! Symmetric encoding uses the fixed neutral outcome name `child`.

use super::super::func::{FlowFn, MirFn};
use super::super::ids::FnRef;
use super::super::ops::{Block, JsonVal, LiveAfter, Stmt, Tail, ToJsonRef, Value};
use super::super::runtime::RuntimeFn;
use super::super::shapes::WireDesc;
use super::super::tydesc::{Leaf, TyDesc};
use super::build::{CodecType, FnPlan};
use super::codec::{
    Stamped, decoder_value, desc_tydesc, lifted_origin, origin, to_json_ref_for, trio_params,
    zero_span,
};
use super::codec_decode::{decoder_flow, missing_slot};
use super::ctx::Ctx;
use super::driver::LowerError;

pub(super) fn codec(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    payload: &WireDesc,
    lifted_refs: &[FnRef],
) -> Result<(Stamped, Stamped), LowerError> {
    let [outcome_ref, payload_ref] = lifted_refs else {
        return Err(missing_slot(ctx, codec_type));
    };
    let (_, _, decoder_ref) = plan.codecs[&codec_type.stem];
    let encoded = encode(ctx, plan, codec_type, payload)?;
    let decoded = decode(
        ctx,
        plan,
        codec_type,
        payload,
        decoder_ref,
        *outcome_ref,
        *payload_ref,
    )?;
    Ok((encoded, decoded))
}

fn encode(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    payload: &WireDesc,
) -> Result<Stamped, LowerError> {
    ctx.reset_vars();
    let value = ctx.fresh_var();
    let object = ctx.fresh_var();
    let child = ctx.binary("child");
    let main = MirFn::Flow(FlowFn {
        origin: origin(codec_type, trio_params(codec_type)),
        name: format!("{}_to_json", codec_type.stem),
        params: vec![value],
        param_tys: vec![codec_type.tydesc.clone()],
        ret_ty: TyDesc::Json,
        body: Block {
            stmts: vec![Stmt::JsonObj {
                dst: object,
                pairs: vec![
                    (
                        "outcome".to_owned(),
                        JsonVal::Encoded {
                            value: Value::Lit(child),
                            via: ToJsonRef::SdkLeaf(Leaf::Str),
                        },
                    ),
                    (
                        "payload".to_owned(),
                        JsonVal::Encoded {
                            value: Value::Var(value),
                            via: to_json_ref_for(ctx, plan, payload)?,
                        },
                    ),
                ],
                span: zero_span(),
            }],
            tail: Tail::Return(Value::Var(object)),
        },
        span: zero_span(),
        degraded_parallel: false,
    });
    Ok(Stamped {
        main,
        lifted: Vec::new(),
    })
}

fn decode(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    payload: &WireDesc,
    decoder_ref: FnRef,
    outcome_ref: FnRef,
    payload_ref: FnRef,
) -> Result<Stamped, LowerError> {
    let main = decode_main(ctx, codec_type, outcome_ref);
    let outcome = decode_outcome(ctx, plan, codec_type, payload, decoder_ref, payload_ref)?;
    let payload = decode_payload(ctx, codec_type, payload, decoder_ref);
    Ok(Stamped {
        main,
        lifted: vec![outcome, payload],
    })
}

fn decode_main(ctx: &mut Ctx<'_>, codec_type: &CodecType, outcome_ref: FnRef) -> MirFn {
    ctx.reset_vars();
    let outcome_name = ctx.binary("outcome");
    let string_decoder = ctx.fresh_var();
    let continuation = ctx.fresh_var();
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
                    dst: continuation,
                    lifted: outcome_ref,
                    captures: Vec::new(),
                    span: zero_span(),
                },
            ],
            tail: Tail::TailRt {
                callee: RuntimeFn::DField,
                args: vec![
                    Value::Lit(outcome_name),
                    Value::Var(string_decoder),
                    Value::Var(continuation),
                ],
            },
        },
    )
}

fn decode_outcome(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    payload: &WireDesc,
    decoder_ref: FnRef,
    payload_ref: FnRef,
) -> Result<MirFn, LowerError> {
    ctx.reset_vars();
    let outcome = ctx.fresh_var();
    let mut stmts = Vec::new();
    let payload_decoder = decoder_value(ctx, plan, payload, &mut stmts)?;
    let continuation = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: continuation,
        lifted: payload_ref,
        captures: Vec::new(),
        span: zero_span(),
    });
    let payload_name = ctx.binary("payload");
    Ok(MirFn::Flow(FlowFn {
        origin: lifted_origin(decoder_ref, 0),
        name: format!("{}_decoder$outcome", codec_type.stem),
        params: vec![outcome],
        param_tys: vec![TyDesc::String],
        ret_ty: TyDesc::Decoder(Box::new(codec_type.tydesc.clone())),
        body: Block {
            stmts,
            tail: Tail::TailRt {
                callee: RuntimeFn::DField,
                args: vec![
                    Value::Lit(payload_name),
                    Value::Var(payload_decoder),
                    Value::Var(continuation),
                ],
            },
        },
        span: zero_span(),
        degraded_parallel: false,
    }))
}

fn decode_payload(
    ctx: &mut Ctx<'_>,
    codec_type: &CodecType,
    payload: &WireDesc,
    decoder_ref: FnRef,
) -> MirFn {
    ctx.reset_vars();
    let value = ctx.fresh_var();
    MirFn::Flow(FlowFn {
        origin: lifted_origin(decoder_ref, 1),
        name: format!("{}_decoder$payload", codec_type.stem),
        params: vec![value],
        param_tys: vec![desc_tydesc(ctx, payload)],
        ret_ty: TyDesc::Decoder(Box::new(codec_type.tydesc.clone())),
        body: Block {
            stmts: Vec::new(),
            tail: Tail::TailRt {
                callee: RuntimeFn::DSuccess,
                args: vec![Value::Var(value)],
            },
        },
        span: zero_span(),
        degraded_parallel: false,
    })
}
