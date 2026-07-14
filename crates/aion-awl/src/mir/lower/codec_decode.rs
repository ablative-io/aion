//! Codec `_decoder` bodies (BC-2b-5): the decode half of the trio, the
//! continuation-taking recipe the reference emitter generates
//! (`emitter/codecs.rs::record_decoder`/`enum_codec`/`union_codec`,
//! `emitter/composites.rs`).
//!
//! Each `use x <- decode.field(...)` desugars to a lifted continuation
//! (`FnOrigin::LiftedClosure`) capturing every earlier field; optional
//! record fields ride `decode.optional_field(name, None,
//! decode.map(inner, Some), cont)` so explicit `null` FAILS and absence is
//! `None` (D4); enum/union unknowns FAIL through `decode.failure` with an
//! OP-built typed default (S5) — never a silently-chosen value.

use super::super::func::{FlowFn, MirFn};
use super::super::ids::{FnRef, Var};
use super::super::ops::{Block, CmpOp, LiveAfter, Stmt, Tail, Test, Value};
use super::super::runtime::RuntimeFn;
use super::super::shapes::{FieldShape, TypeShape, WireDesc};
use super::super::tydesc::{Leaf, TyDesc};
use super::build::{CodecType, FnPlan};
use super::codec::{Stamped, decoder_value, desc_tydesc, lifted_origin, zero_span};
use super::ctx::Ctx;
use super::driver::LowerError;

pub(super) fn decoder_flow(codec_type: &CodecType, body: Block) -> MirFn {
    MirFn::Flow(FlowFn {
        origin: super::codec::origin(codec_type, super::codec::trio_params(codec_type)),
        name: format!("{}_decoder", codec_type.stem),
        params: Vec::new(),
        param_tys: Vec::new(),
        ret_ty: TyDesc::Decoder(Box::new(codec_type.tydesc.clone())),
        body,
        span: zero_span(),
        degraded_parallel: false,
    })
}

pub(super) fn missing_slot(ctx: &Ctx<'_>, codec_type: &CodecType) -> LowerError {
    LowerError::new(
        ctx.emitter.document.span,
        format!(
            "codec `{}` reserved fewer lifted slots than its decoder needs",
            codec_type.stem
        ),
    )
}

/// Record `_decoder`: nested `decode.field`/`decode.optional_field`
/// continuations ending in `decode.success(Record(...))`.
pub(super) fn record(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    shape: &TypeShape,
    dec_refs: &[FnRef],
) -> Result<Stamped, LowerError> {
    let TypeShape::Record { tag, fields, .. } = shape else {
        return Err(LowerError::new(
            ctx.emitter.document.span,
            format!("codec `{}` is not a record shape", codec_type.stem),
        ));
    };
    let tag = *tag;
    let fields = fields.clone();
    let has_optional = fields.iter().any(|field| field.optional);
    let (some_ref, cont_refs) = if has_optional {
        let some_ref = dec_refs.first().copied();
        (
            Some(some_ref.ok_or_else(|| missing_slot(ctx, codec_type))?),
            dec_refs.get(1..).unwrap_or_default(),
        )
    } else {
        (None, dec_refs)
    };
    if cont_refs.len() != fields.len() {
        return Err(missing_slot(ctx, codec_type));
    }

    // The top-level decoder: empty records succeed with the bare tag; a
    // fielded record opens the continuation chain at field 0.
    ctx.reset_vars();
    let body = if fields.is_empty() {
        Block {
            stmts: Vec::new(),
            tail: Tail::TailRt {
                callee: RuntimeFn::DSuccess,
                args: vec![Value::Atom(tag)],
            },
        }
    } else {
        let mut stmts = Vec::new();
        let tail = field_step(
            ctx,
            plan,
            &fields[0],
            cont_refs[0],
            some_ref,
            Vec::new(),
            &mut stmts,
        )?;
        Block { stmts, tail }
    };
    let main = decoder_flow(codec_type, body);

    let mut lifted = Vec::new();
    let (_, _, decoder_ref) = plan.codecs[&codec_type.stem];
    let mut lifted_index = 0_u32;
    if let Some(reference) = some_ref {
        let _ = reference;
        lifted.push(some_wrapper(ctx, codec_type, decoder_ref, lifted_index));
        lifted_index += 1;
    }
    for (position, _) in fields.iter().enumerate() {
        lifted.push(record_cont(
            ctx,
            plan,
            codec_type,
            tag,
            &fields,
            position,
            cont_refs,
            some_ref,
            decoder_ref,
            lifted_index,
        )?);
        lifted_index += 1;
    }
    Ok(Stamped { main, lifted })
}

/// One `decode.field`/`decode.optional_field` step for field `k`, with the
/// continuation closure capturing every earlier field var.
fn field_step(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    field: &FieldShape,
    cont_ref: FnRef,
    some_ref: Option<FnRef>,
    captures: Vec<Value>,
    stmts: &mut Vec<Stmt>,
) -> Result<Tail, LowerError> {
    let name_lit = ctx.binary(&field.awl_name);
    if field.optional {
        let WireDesc::Nullable(inner) = &field.desc else {
            return Err(LowerError::new(
                ctx.emitter.document.span,
                format!(
                    "optional field `{}` has no nullable descriptor",
                    field.awl_name
                ),
            ));
        };
        let inner = (**inner).clone();
        let inner_decoder = decoder_value(ctx, plan, &inner, stmts)?;
        let some_ref = some_ref.ok_or_else(|| {
            LowerError::new(
                ctx.emitter.document.span,
                format!(
                    "optional field `{}` has no Some-wrapper slot",
                    field.awl_name
                ),
            )
        })?;
        let some_closure = ctx.fresh_var();
        stmts.push(Stmt::MakeClosure {
            dst: some_closure,
            lifted: some_ref,
            captures: Vec::new(),
            span: zero_span(),
        });
        let mapped = ctx.fresh_var();
        stmts.push(Stmt::CallRt {
            dst: Some(mapped),
            callee: RuntimeFn::DMap,
            args: vec![Value::Var(inner_decoder), Value::Var(some_closure)],
            live_after: LiveAfter::default(),
            span: zero_span(),
        });
        let cont = ctx.fresh_var();
        stmts.push(Stmt::MakeClosure {
            dst: cont,
            lifted: cont_ref,
            captures,
            span: zero_span(),
        });
        let none_atom = ctx.atom("none");
        Ok(Tail::TailRt {
            callee: RuntimeFn::DOptionalField,
            args: vec![
                Value::Lit(name_lit),
                Value::Atom(none_atom),
                Value::Var(mapped),
                Value::Var(cont),
            ],
        })
    } else {
        let decoder = decoder_value(ctx, plan, &field.desc, stmts)?;
        let cont = ctx.fresh_var();
        stmts.push(Stmt::MakeClosure {
            dst: cont,
            lifted: cont_ref,
            captures,
            span: zero_span(),
        });
        Ok(Tail::TailRt {
            callee: RuntimeFn::DField,
            args: vec![Value::Lit(name_lit), Value::Var(decoder), Value::Var(cont)],
        })
    }
}

/// The continuation receiving field `k` (declared arg 0) with fields
/// `0..k-1` appended as captures: either the next field step or the final
/// `decode.success(Record(...))`.
#[expect(
    clippy::too_many_arguments,
    reason = "one stamping site; the values are the continuation's full identity"
)]
fn record_cont(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    tag: super::super::ids::AtomRef,
    fields: &[FieldShape],
    position: usize,
    cont_refs: &[FnRef],
    some_ref: Option<FnRef>,
    host: FnRef,
    lifted_index: u32,
) -> Result<MirFn, LowerError> {
    ctx.reset_vars();
    // Physical params: the decoded field k, then fields 0..k-1 (captures).
    let arg = ctx.fresh_var();
    let caps: Vec<Var> = (0..position).map(|_| ctx.fresh_var()).collect();
    // Field j's var: caps[j] for j < position, `arg` for j == position.
    let field_var = |j: usize| -> Var { if j == position { arg } else { caps[j] } };

    let body = if position + 1 == fields.len() {
        let record = ctx.fresh_var();
        let args: Vec<Value> = (0..fields.len())
            .map(|j| Value::Var(field_var(j)))
            .collect();
        Block {
            stmts: vec![Stmt::RecordNew {
                dst: record,
                tag,
                args,
                span: zero_span(),
            }],
            tail: Tail::TailRt {
                callee: RuntimeFn::DSuccess,
                args: vec![Value::Var(record)],
            },
        }
    } else {
        let next = position + 1;
        let captures: Vec<Value> = (0..next).map(|j| Value::Var(field_var(j))).collect();
        let mut stmts = Vec::new();
        let tail = field_step(
            ctx,
            plan,
            &fields[next],
            cont_refs[next],
            some_ref,
            captures,
            &mut stmts,
        )?;
        Block { stmts, tail }
    };

    let mut param_tys = vec![desc_tydesc(ctx, &fields[position].desc)];
    for field in &fields[..position] {
        param_tys.push(desc_tydesc(ctx, &field.desc));
    }
    let mut params = vec![arg];
    params.extend(caps);
    Ok(MirFn::Flow(FlowFn {
        origin: lifted_origin(host, lifted_index),
        name: format!("{}_decoder${position}", codec_type.stem),
        params,
        param_tys,
        ret_ty: TyDesc::Decoder(Box::new(codec_type.tydesc.clone())),
        body,
        span: zero_span(),
        degraded_parallel: false,
    }))
}

/// The shared `Some` constructor fun value `decode.map(inner, Some)` rides.
fn some_wrapper(
    ctx: &mut Ctx<'_>,
    codec_type: &CodecType,
    host: FnRef,
    lifted_index: u32,
) -> MirFn {
    ctx.reset_vars();
    let value = ctx.fresh_var();
    let some_atom = ctx.atom("some");
    let wrapped = ctx.fresh_var();
    MirFn::Flow(FlowFn {
        origin: lifted_origin(host, lifted_index),
        name: format!("{}_decoder$some", codec_type.stem),
        params: vec![value],
        param_tys: vec![TyDesc::Dynamic],
        ret_ty: TyDesc::Option(Box::new(TyDesc::Dynamic)),
        body: Block {
            stmts: vec![Stmt::RecordNew {
                dst: wrapped,
                tag: some_atom,
                args: vec![Value::Var(value)],
                span: zero_span(),
            }],
            tail: Tail::Return(Value::Var(wrapped)),
        },
        span: zero_span(),
        degraded_parallel: false,
    })
}

/// Enum `_decoder`: `decode.then(decode.string)` into a lifted case over the
/// variant strings; unknown values FAIL with the first variant + type name.
pub(super) fn enum_shape(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    shape: &TypeShape,
    lifted_refs: &[FnRef],
) -> Result<Stamped, LowerError> {
    let TypeShape::Enum { name, variants } = shape else {
        return Err(LowerError::new(
            ctx.emitter.document.span,
            format!("codec `{}` is not an enum shape", codec_type.stem),
        ));
    };
    let name = name.clone();
    let variants = variants.clone();
    let cont_ref = lifted_refs
        .first()
        .copied()
        .ok_or_else(|| missing_slot(ctx, codec_type))?;
    let first_ctor = variants.first().map(|(ctor, _)| *ctor).ok_or_else(|| {
        LowerError::new(
            ctx.emitter.document.span,
            format!("enum `{name}` has no variants"),
        )
    })?;

    ctx.reset_vars();
    let string_decoder = ctx.fresh_var();
    let cont = ctx.fresh_var();
    let main = decoder_flow(
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
                    lifted: cont_ref,
                    captures: Vec::new(),
                    span: zero_span(),
                },
            ],
            tail: Tail::TailRt {
                callee: RuntimeFn::DThen,
                args: vec![Value::Var(string_decoder), Value::Var(cont)],
            },
        },
    );

    // The continuation: every variant string tested, unknown values fail.
    ctx.reset_vars();
    let value = ctx.fresh_var();
    let type_lit = ctx.binary(&name);
    let mut tail_block = Block {
        stmts: Vec::new(),
        tail: Tail::TailRt {
            callee: RuntimeFn::DFailure,
            args: vec![Value::Atom(first_ctor), Value::Lit(type_lit)],
        },
    };
    for (ctor, json_name) in variants.iter().rev() {
        let variant_lit = ctx.binary(json_name);
        tail_block = Block {
            stmts: Vec::new(),
            tail: Tail::If {
                test: Test::Cmp {
                    op: CmpOp::Eq,
                    lhs: Value::Var(value),
                    rhs: Value::Lit(variant_lit),
                },
                then_block: Box::new(Block {
                    stmts: Vec::new(),
                    tail: Tail::TailRt {
                        callee: RuntimeFn::DSuccess,
                        args: vec![Value::Atom(*ctor)],
                    },
                }),
                else_block: Box::new(tail_block),
                span: zero_span(),
            },
        };
    }
    let (_, _, decoder_ref) = plan.codecs[&codec_type.stem];
    let cont_fn = MirFn::Flow(FlowFn {
        origin: lifted_origin(decoder_ref, 0),
        name: format!("{}_decoder$then", codec_type.stem),
        params: vec![value],
        param_tys: vec![TyDesc::String],
        ret_ty: TyDesc::Decoder(Box::new(codec_type.tydesc.clone())),
        body: tail_block,
        span: zero_span(),
        degraded_parallel: false,
    });
    Ok(Stamped {
        main,
        lifted: vec![cont_fn],
    })
}

/// Composite `_decoder`: `decode.list(inner)` / `decode.optional(inner)`.
pub(super) fn composite(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    desc: &WireDesc,
) -> Result<Stamped, LowerError> {
    let (callee, inner) = match desc {
        WireDesc::List(inner) => (RuntimeFn::DList, (**inner).clone()),
        WireDesc::Nullable(inner) => (RuntimeFn::DOptional, (**inner).clone()),
        _ => {
            return Err(LowerError::new(
                ctx.emitter.document.span,
                format!("composite codec `{}` is not a list/option", codec_type.stem),
            ));
        }
    };
    ctx.reset_vars();
    let mut stmts = Vec::new();
    let inner_decoder = decoder_value(ctx, plan, &inner, &mut stmts)?;
    Ok(Stamped {
        main: decoder_flow(
            codec_type,
            Block {
                stmts,
                tail: Tail::TailRt {
                    callee,
                    args: vec![Value::Var(inner_decoder)],
                },
            },
        ),
        lifted: Vec::new(),
    })
}
