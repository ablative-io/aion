//! Codec-trio stamping (S8): each reachable record/enum/union expands into
//! `_codec`/`_to_json`/`_decoder` `FlowFn`s carrying `FnOrigin::CodecTemplate`
//! provenance and faithful signatures (the sidecar-projection source, S2).
//!
//! BC-2 scope note: the `_codec` composer and record `_to_json` bodies (for
//! required leaf/`Ref` fields) follow the §3 recipe; enum/union `_to_json` and
//! every `_decoder` body are structural placeholders (visible in goldens,
//! never silent) pending the lifted-closure decoder-continuation recipe. See
//! `AWL-BC-IR.md` "BC-2 implementation status" for the full pending list.

use super::super::func::{CodecTemplateKind, FlowFn, FnOrigin, MirFn, TrioParams, TypeShapeRef};
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{Block, JsonVal, Stmt, Tail, ToJsonRef, Value};
use super::super::shapes::TypeShape;
use super::super::tydesc::TyDesc;
use super::build::FnPlan;
use super::ctx::Ctx;

/// Stamp the three trio functions for one codec type into `functions`.
pub(super) fn trio(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    types: &[TypeShape],
    codec_type: &super::build::CodecType,
    functions: &mut Vec<MirFn>,
) {
    let (_codec_ref, to_json_ref, decoder_ref) = plan.codecs[&codec_type.stem];
    let trio_params = trio_params(codec_type);
    functions.push(codec_fn(
        ctx,
        codec_type,
        to_json_ref,
        decoder_ref,
        trio_params.clone(),
    ));
    functions.push(to_json_fn(
        ctx,
        plan,
        types,
        codec_type,
        trio_params.clone(),
    ));
    functions.push(decoder_fn(ctx, codec_type, trio_params));
}

fn trio_params(codec_type: &super::build::CodecType) -> TrioParams {
    let shape = TypeShapeRef(u16::try_from(codec_type.shape_index).unwrap_or(u16::MAX));
    match codec_type.kind {
        CodecTemplateKind::RecordTrio => TrioParams::Record { shape },
        CodecTemplateKind::EnumTrio => TrioParams::Enum { shape },
        CodecTemplateKind::UnionTrio => TrioParams::Union { shape },
        CodecTemplateKind::CompositeTrio => TrioParams::Composite {
            desc: super::super::shapes::WireDesc::Nil,
        },
    }
}

fn origin(codec_type: &super::build::CodecType, params: TrioParams) -> FnOrigin {
    FnOrigin::CodecTemplate {
        kind: codec_type.kind,
        subject: codec_type.subject.clone(),
        params,
    }
}

fn zero_span() -> Span {
    Span::zero()
}

/// `<stem>_codec/0`: `json_codec(<to_json>, <decoder>)` — the §3 composer.
fn codec_fn(
    ctx: &mut Ctx<'_>,
    codec_type: &super::build::CodecType,
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
            live_after: super::super::ops::LiveAfter::default(),
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
                callee: super::super::runtime::RuntimeFn::JsonCodec,
                args: vec![Value::Var(closure), Value::Var(decoder)],
            },
        },
        span: zero_span(),
        degraded_parallel: false,
    })
}

/// `<stem>_to_json/1`. Record bodies follow the §3 recipe; enum/union bodies
/// are structural placeholders (reported).
fn to_json_fn(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    types: &[TypeShape],
    codec_type: &super::build::CodecType,
    params: TrioParams,
) -> MirFn {
    ctx.reset_vars();
    let subject = ctx.fresh_var();
    let shape = &types[codec_type.shape_index];
    let body = match shape {
        TypeShape::Record { .. } => record_to_json(ctx, plan, subject, shape),
        TypeShape::Enum { .. } | TypeShape::Union { .. } => placeholder_json(ctx),
    };
    MirFn::Flow(FlowFn {
        origin: origin(codec_type, params),
        name: format!("{}_to_json", codec_type.stem),
        params: vec![subject],
        param_tys: vec![codec_type.tydesc.clone()],
        ret_ty: TyDesc::Json,
        body,
        span: zero_span(),
        degraded_parallel: false,
    })
}

fn record_to_json(ctx: &mut Ctx<'_>, plan: &FnPlan, subject: Var, shape: &TypeShape) -> Block {
    let TypeShape::Record { fields, .. } = shape else {
        return placeholder_json(ctx);
    };
    let mut stmts = Vec::new();
    let mut pairs = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let value = ctx.fresh_var();
        stmts.push(Stmt::FieldGet {
            dst: value,
            base: Value::Var(subject),
            index: u16::try_from(index + 1).unwrap_or(u16::MAX),
            span: zero_span(),
        });
        pairs.push((
            field.awl_name.clone(),
            JsonVal::Encoded {
                value: Value::Var(value),
                via: to_json_ref(plan, &field.desc),
            },
        ));
    }
    let object = ctx.fresh_var();
    stmts.push(Stmt::JsonObj {
        dst: object,
        pairs,
        span: zero_span(),
    });
    Block {
        stmts,
        tail: Tail::Return(Value::Var(object)),
    }
}

/// A structural `json.object([])` body (BC-2 placeholder for enum/union
/// `_to_json`, visible in goldens).
fn placeholder_json(ctx: &mut Ctx<'_>) -> Block {
    let object = ctx.fresh_var();
    Block {
        stmts: vec![Stmt::JsonObj {
            dst: object,
            pairs: Vec::new(),
            span: zero_span(),
        }],
        tail: Tail::Return(Value::Var(object)),
    }
}

/// `<stem>_decoder/0`: a structural `decode.success(Nil)` placeholder (BC-2
/// scope; the full continuation recipe is reported as deferred).
fn decoder_fn(
    ctx: &mut Ctx<'_>,
    codec_type: &super::build::CodecType,
    params: TrioParams,
) -> MirFn {
    ctx.reset_vars();
    MirFn::Flow(FlowFn {
        origin: origin(codec_type, params),
        name: format!("{}_decoder", codec_type.stem),
        params: Vec::new(),
        param_tys: Vec::new(),
        ret_ty: TyDesc::Decoder(Box::new(codec_type.tydesc.clone())),
        body: Block {
            stmts: Vec::new(),
            tail: Tail::TailRt {
                callee: super::super::runtime::RuntimeFn::DSuccess,
                args: vec![Value::Nil],
            },
        },
        span: zero_span(),
        degraded_parallel: false,
    })
}

/// The `to_json` function a field's wire descriptor flows through.
fn to_json_ref(plan: &FnPlan, desc: &super::super::shapes::WireDesc) -> ToJsonRef {
    use super::super::shapes::WireDesc;
    match desc {
        WireDesc::Bool => ToJsonRef::SdkLeaf(super::super::tydesc::Leaf::Bool),
        WireDesc::Int => ToJsonRef::SdkLeaf(super::super::tydesc::Leaf::Int),
        WireDesc::Float => ToJsonRef::SdkLeaf(super::super::tydesc::Leaf::Float),
        WireDesc::Str => ToJsonRef::SdkLeaf(super::super::tydesc::Leaf::Str),
        WireDesc::Nil | WireDesc::List(_) | WireDesc::Nullable(_) => {
            ToJsonRef::SdkLeaf(super::super::tydesc::Leaf::Nil)
        }
        WireDesc::Ref(name) => plan.codecs.get(&crate::emitter::snake(name)).map_or(
            ToJsonRef::SdkLeaf(super::super::tydesc::Leaf::Nil),
            |trio| ToJsonRef::Local(trio.1),
        ),
    }
}
