//! Codec-trio stamping (S8): each reachable record/enum/union/composite
//! expands into `_codec`/`_to_json`/`_decoder` `FlowFn`s carrying
//! `FnOrigin::CodecTemplate` provenance and faithful signatures (the
//! sidecar-projection source, S2), plus its lifted continuation/helper
//! functions (`FnOrigin::LiftedClosure`).
//!
//! BC-2b-5: every body follows the §3 recipe and the reference emitter
//! exactly (`emitter/codecs.rs`, `emitter/composites.rs`) — record encode
//! preserves declaration order and OMITS absent optional fields (D4), record
//! decode nests `decode.field`/`decode.optional_field` continuations, enums
//! encode JSON strings and FAIL on unknown variants, the outcome union
//! encodes `{outcome, payload}` and fails unknown outcomes with an OP-built
//! typed default (S5), and composites ride `json.array`/`json.nullable` +
//! `decode.list`/`decode.optional`. No placeholder bodies remain.

use super::super::func::{CodecTemplateKind, FlowFn, FnOrigin, MirFn, TrioParams, TypeShapeRef};
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{Block, LiveAfter, Stmt, Tail, ToJsonRef, Value};
use super::super::runtime::RuntimeFn;
use super::super::shapes::{TypeShape, WireDesc};
use super::super::tydesc::{Leaf, TyDesc};
use super::build::{CodecPayload, CodecType, FnPlan};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::{codec_decode as decode, codec_encode as encode};

/// The lifted-function slots one codec type reserves, in the canonical order
/// `trio` pushes them:
/// - record: one optional-field pair fn per optional field (field order),
///   one shared `Some` wrapper when any field is optional, then one decoder
///   continuation per field;
/// - enum: the one `decode.then` continuation;
/// - union: the outcome continuation, then one payload continuation per arm;
/// - composite: one leaf `to_json` item wrapper when the inner is a leaf.
pub(super) fn lifted_count(codec_type: &CodecType, types: &[TypeShape]) -> usize {
    match (&codec_type.payload, codec_type.shape(types)) {
        (CodecPayload::Shape(_), Some(TypeShape::Record { fields, .. })) => {
            let optional = fields.iter().filter(|field| field.optional).count();
            optional + usize::from(optional > 0) + fields.len()
        }
        (CodecPayload::Shape(_), Some(TypeShape::Enum { .. })) => 1,
        (CodecPayload::Shape(_), Some(TypeShape::Union { arms, .. })) => 1 + arms.len(),
        (CodecPayload::Composite(desc), _) => match desc {
            WireDesc::List(inner) | WireDesc::Nullable(inner) => {
                usize::from(leaf_of_desc(inner).is_some())
            }
            _ => 0,
        },
        (CodecPayload::Shape(_), None) => 0,
    }
}

/// The two halves of one codec function: the trio member itself plus the
/// lifted helper functions it owns, in their reserved slot order.
pub(super) struct Stamped {
    pub(super) main: MirFn,
    pub(super) lifted: Vec<MirFn>,
}

/// Stamp the trio + lifted functions for one codec type into `functions`, in
/// exact slot order (`_codec`, `_to_json`, `_decoder`, encode-side lifted…,
/// decode-side lifted…).
pub(super) fn trio(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    types: &[TypeShape],
    codec_type: &CodecType,
    functions: &mut Vec<MirFn>,
) -> Result<(), LowerError> {
    let (_codec_ref, to_json_ref, decoder_ref) = plan.codecs[&codec_type.stem];
    let trio_params = trio_params(codec_type);
    let composer = codec_fn(
        ctx,
        codec_type,
        to_json_ref,
        decoder_ref,
        trio_params.clone(),
    );
    let lifted_refs = plan
        .codec_lifted
        .get(&codec_type.stem)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let (enc, dec) = match &codec_type.payload {
        CodecPayload::Shape(index) => {
            let shape = types.get(*index).ok_or_else(|| {
                LowerError::new(
                    ctx.emitter.document.span,
                    format!("codec `{}` references no type shape", codec_type.stem),
                )
            })?;
            match shape {
                TypeShape::Record { fields, .. } => {
                    let enc_count = fields.iter().filter(|field| field.optional).count();
                    let (enc_refs, dec_refs) =
                        split_lifted(ctx, codec_type, lifted_refs, enc_count)?;
                    (
                        encode::record(ctx, plan, codec_type, shape, trio_params, enc_refs)?,
                        decode::record(ctx, plan, codec_type, shape, dec_refs)?,
                    )
                }
                TypeShape::Enum { .. } => (
                    encode::enum_shape(ctx, codec_type, shape, trio_params),
                    decode::enum_shape(ctx, plan, codec_type, shape, lifted_refs)?,
                ),
                TypeShape::Union { .. } => (
                    encode::union(ctx, plan, codec_type, shape, trio_params)?,
                    super::codec_decode_union::union(
                        ctx,
                        plan,
                        types,
                        codec_type,
                        shape,
                        lifted_refs,
                    )?,
                ),
            }
        }
        CodecPayload::Composite(desc) => {
            let desc = desc.clone();
            (
                encode::composite(ctx, plan, codec_type, &desc, trio_params, lifted_refs)?,
                decode::composite(ctx, plan, codec_type, &desc)?,
            )
        }
    };
    functions.push(composer);
    functions.push(enc.main);
    functions.push(dec.main);
    functions.extend(enc.lifted);
    functions.extend(dec.lifted);
    Ok(())
}

/// Split a codec type's reserved lifted slots into the encode-side and
/// decode-side halves without panicking on a plan mismatch.
fn split_lifted<'r>(
    ctx: &Ctx<'_>,
    codec_type: &CodecType,
    lifted: &'r [FnRef],
    enc_count: usize,
) -> Result<(&'r [FnRef], &'r [FnRef]), LowerError> {
    if enc_count <= lifted.len() {
        Ok(lifted.split_at(enc_count))
    } else {
        Err(LowerError::new(
            ctx.emitter.document.span,
            format!(
                "codec `{}` reserved {} lifted slots but needs {enc_count} encode-side",
                codec_type.stem,
                lifted.len()
            ),
        ))
    }
}

pub(super) fn trio_params(codec_type: &CodecType) -> TrioParams {
    match &codec_type.payload {
        CodecPayload::Shape(index) => {
            let shape = TypeShapeRef(u16::try_from(*index).unwrap_or(u16::MAX));
            match codec_type.kind {
                CodecTemplateKind::EnumTrio => TrioParams::Enum { shape },
                CodecTemplateKind::UnionTrio => TrioParams::Union { shape },
                _ => TrioParams::Record { shape },
            }
        }
        CodecPayload::Composite(desc) => TrioParams::Composite { desc: desc.clone() },
    }
}

pub(super) fn origin(codec_type: &CodecType, params: TrioParams) -> FnOrigin {
    FnOrigin::CodecTemplate {
        kind: codec_type.kind,
        subject: codec_type.subject.clone(),
        params,
    }
}

pub(super) fn lifted_origin(host: FnRef, index: u32) -> FnOrigin {
    FnOrigin::LiftedClosure { host, index }
}

pub(super) fn zero_span() -> Span {
    Span::zero()
}

/// `<stem>_codec/0`: `json_codec(<to_json>, <decoder>)` — the §3 composer.
fn codec_fn(
    ctx: &mut Ctx<'_>,
    codec_type: &CodecType,
    to_json_ref: FnRef,
    decoder_ref: FnRef,
    params: TrioParams,
) -> MirFn {
    ctx.reset_vars();
    let closure = ctx.fresh_var();
    let decoder = ctx.fresh_var();
    let stmts = vec![
        Stmt::MakeClosure {
            dst: closure,
            lifted: to_json_ref,
            captures: Vec::new(),
            span: zero_span(),
        },
        Stmt::CallLocal {
            dst: Some(decoder),
            callee: decoder_ref,
            args: Vec::new(),
            // S14 fills this: `closure` is live across the decoder call
            // (both feed the `json_codec` tail).
            live_after: LiveAfter::default(),
            span: zero_span(),
        },
    ];
    MirFn::Flow(FlowFn {
        origin: origin(codec_type, params),
        name: format!("{}_codec", codec_type.stem),
        params: Vec::new(),
        param_tys: Vec::new(),
        ret_ty: TyDesc::Codec(Box::new(codec_type.tydesc.clone())),
        body: Block {
            stmts,
            tail: Tail::TailRt {
                callee: RuntimeFn::JsonCodec,
                args: vec![Value::Var(closure), Value::Var(decoder)],
            },
        },
        span: zero_span(),
        degraded_parallel: false,
    })
}

// ---- shared wire-descriptor helpers -------------------------------------

/// The leaf a wire descriptor resolves to, when it is one.
pub(super) fn leaf_of_desc(desc: &WireDesc) -> Option<Leaf> {
    match desc {
        WireDesc::Bool => Some(Leaf::Bool),
        WireDesc::Int => Some(Leaf::Int),
        WireDesc::Float => Some(Leaf::Float),
        WireDesc::Str => Some(Leaf::Str),
        WireDesc::Nil => Some(Leaf::Nil),
        WireDesc::List(_) | WireDesc::Nullable(_) | WireDesc::Ref(_) => None,
    }
}

/// The registry stem a non-leaf wire descriptor's trio lives under — the
/// exact mirror of the reference `TypeEnv::codec_name`.
pub(super) fn desc_stem(desc: &WireDesc) -> String {
    match desc {
        WireDesc::Bool => "bool".to_owned(),
        WireDesc::Int => "int".to_owned(),
        WireDesc::Float => "float".to_owned(),
        WireDesc::Str => "string".to_owned(),
        WireDesc::Nil => "nil".to_owned(),
        WireDesc::List(inner) => format!("list_{}", desc_stem(inner)),
        WireDesc::Nullable(inner) => format!("option_{}", desc_stem(inner)),
        WireDesc::Ref(name) => crate::emitter::snake(name),
    }
}

/// The `TyDesc` a wire descriptor's decoded value carries.
pub(super) fn desc_tydesc(ctx: &Ctx<'_>, desc: &WireDesc) -> TyDesc {
    match desc {
        WireDesc::Bool => TyDesc::Bool,
        WireDesc::Int => TyDesc::Int,
        WireDesc::Float => TyDesc::Float,
        WireDesc::Str => TyDesc::String,
        WireDesc::Nil => TyDesc::Nil,
        WireDesc::List(inner) => TyDesc::List(Box::new(desc_tydesc(ctx, inner))),
        WireDesc::Nullable(inner) => TyDesc::Option(Box::new(desc_tydesc(ctx, inner))),
        WireDesc::Ref(name) => TyDesc::Custom {
            module: ctx.module_name.clone(),
            name: name.clone(),
            params: Vec::new(),
        },
    }
}

/// The registered trio for a wire descriptor — a miss is a lowering error
/// (S1), never a Nil fallback.
pub(super) fn desc_trio(
    ctx: &Ctx<'_>,
    plan: &FnPlan,
    desc: &WireDesc,
) -> Result<(FnRef, FnRef, FnRef), LowerError> {
    super::build::registered_codec(plan, ctx, &desc_stem(desc))
}

/// The `to_json` function a wire descriptor's values flow through.
pub(super) fn to_json_ref_for(
    ctx: &Ctx<'_>,
    plan: &FnPlan,
    desc: &WireDesc,
) -> Result<ToJsonRef, LowerError> {
    match leaf_of_desc(desc) {
        Some(leaf) => Ok(ToJsonRef::SdkLeaf(leaf)),
        None => Ok(ToJsonRef::Local(desc_trio(ctx, plan, desc)?.1)),
    }
}

/// Emit the statement producing a DECODER VALUE for a wire descriptor
/// (`awlc.<leaf>_decoder()` or the local trio's `<stem>_decoder()`).
pub(super) fn decoder_value(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    desc: &WireDesc,
    stmts: &mut Vec<Stmt>,
) -> Result<Var, LowerError> {
    let dst = ctx.fresh_var();
    match leaf_of_desc(desc) {
        Some(leaf) => stmts.push(Stmt::CallRt {
            dst: Some(dst),
            callee: RuntimeFn::LeafDecoder(leaf),
            args: Vec::new(),
            live_after: LiveAfter::default(),
            span: zero_span(),
        }),
        None => stmts.push(Stmt::CallLocal {
            dst: Some(dst),
            callee: desc_trio(ctx, plan, desc)?.2,
            args: Vec::new(),
            live_after: LiveAfter::default(),
            span: zero_span(),
        }),
    }
    Ok(dst)
}

/// Emit the statement ENCODING one value through a wire descriptor's
/// `to_json`, returning the JSON var.
pub(super) fn encode_value(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    desc: &WireDesc,
    value: Value,
    stmts: &mut Vec<Stmt>,
) -> Result<Var, LowerError> {
    let dst = ctx.fresh_var();
    match to_json_ref_for(ctx, plan, desc)? {
        ToJsonRef::SdkLeaf(leaf) => stmts.push(Stmt::CallRt {
            dst: Some(dst),
            callee: RuntimeFn::LeafToJson(leaf),
            args: vec![value],
            live_after: LiveAfter::default(),
            span: zero_span(),
        }),
        ToJsonRef::Local(reference) => stmts.push(Stmt::CallLocal {
            dst: Some(dst),
            callee: reference,
            args: vec![value],
            live_after: LiveAfter::default(),
            span: zero_span(),
        }),
    }
    Ok(dst)
}
