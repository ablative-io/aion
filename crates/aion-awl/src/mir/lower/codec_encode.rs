//! Codec `_to_json` bodies (BC-2b-5): the encode half of the trio, matching
//! the reference emitter exactly (`emitter/codecs.rs::record_to_json`/
//! `enum_codec`/`union_codec`, `emitter/composites.rs`).
//!
//! D4 optional record fields: encoding OMITS an absent field entirely — each
//! optional field's `[#(name, json)] / []` arm is a lifted pair function
//! (`If` is tail-position-only in MIR), flattened with `gleam@list:flatten`
//! before `gleam@json:object`.

use super::super::func::{FlowFn, MirFn, TrioParams};
use super::super::ids::{FnRef, Var};
use super::super::ops::{Block, JsonVal, LiveAfter, Stmt, Tail, Test, Value};
use super::super::runtime::RuntimeFn;
use super::super::shapes::{FieldShape, TypeShape, WireDesc};
use super::super::tydesc::TyDesc;
use super::build::{CodecType, FnPlan};
use super::codec::{
    Stamped, desc_tydesc, encode_value, leaf_of_desc, lifted_origin, origin, to_json_ref_for,
    zero_span,
};
use super::ctx::Ctx;
use super::driver::LowerError;

fn flow(codec_type: &CodecType, params: TrioParams, subject: Var, body: Block) -> MirFn {
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

/// Record `_to_json`: declaration order; static `json.object` when no field
/// is optional, the flatten recipe otherwise.
pub(super) fn record(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    shape: &TypeShape,
    params: TrioParams,
    enc_refs: &[FnRef],
) -> Result<Stamped, LowerError> {
    let TypeShape::Record { fields, .. } = shape else {
        return Err(LowerError::new(
            ctx.emitter.document.span,
            format!("codec `{}` is not a record shape", codec_type.stem),
        ));
    };
    let fields = fields.clone();
    let has_optional = fields.iter().any(|field| field.optional);
    ctx.reset_vars();
    let subject = ctx.fresh_var();
    let body = if has_optional {
        optional_record_body(ctx, plan, subject, &fields, enc_refs)?
    } else {
        static_record_body(ctx, plan, subject, &fields)?
    };
    let main = flow(codec_type, params, subject, body);
    let mut lifted = Vec::new();
    let (_, to_json_ref, _) = plan.codecs[&codec_type.stem];
    let mut next_ref = enc_refs.iter();
    for (index, field) in fields.iter().enumerate() {
        if !field.optional {
            continue;
        }
        let reference = next_ref.next().copied().ok_or_else(|| {
            LowerError::new(
                ctx.emitter.document.span,
                format!("codec `{}` has no slot for a pair fn", codec_type.stem),
            )
        })?;
        let position = u32::try_from(lifted.len()).unwrap_or(u32::MAX);
        let _ = reference;
        lifted.push(pair_fn(
            ctx,
            plan,
            codec_type,
            field,
            index,
            to_json_ref,
            position,
        )?);
    }
    Ok(Stamped { main, lifted })
}

fn static_record_body(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    subject: Var,
    fields: &[FieldShape],
) -> Result<Block, LowerError> {
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
                via: to_json_ref_for(ctx, plan, &field.desc)?,
            },
        ));
    }
    let object = ctx.fresh_var();
    stmts.push(Stmt::JsonObj {
        dst: object,
        pairs,
        span: zero_span(),
    });
    Ok(Block {
        stmts,
        tail: Tail::Return(Value::Var(object)),
    })
}

/// `json.object(list.flatten([...]))`: required fields build one-element
/// pair lists inline; optional fields call their lifted pair fn.
fn optional_record_body(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    subject: Var,
    fields: &[FieldShape],
    enc_refs: &[FnRef],
) -> Result<Block, LowerError> {
    let mut stmts = Vec::new();
    let mut pair_lists = Vec::new();
    let mut next_ref = enc_refs.iter();
    for (index, field) in fields.iter().enumerate() {
        let value = ctx.fresh_var();
        stmts.push(Stmt::FieldGet {
            dst: value,
            base: Value::Var(subject),
            index: u16::try_from(index + 1).unwrap_or(u16::MAX),
            span: zero_span(),
        });
        if field.optional {
            let pair_ref = next_ref.next().copied().ok_or_else(|| {
                LowerError::new(
                    ctx.emitter.document.span,
                    format!("no pair-fn slot for optional field `{}`", field.awl_name),
                )
            })?;
            let list = ctx.fresh_var();
            stmts.push(Stmt::CallLocal {
                dst: Some(list),
                callee: pair_ref,
                args: vec![Value::Var(value)],
                live_after: LiveAfter::default(),
                span: zero_span(),
            });
            pair_lists.push(Value::Var(list));
        } else {
            let encoded = encode_value(ctx, plan, &field.desc, Value::Var(value), &mut stmts)?;
            let name_lit = ctx.binary(&field.awl_name);
            let pair = ctx.fresh_var();
            stmts.push(Stmt::TupleNew {
                dst: pair,
                items: vec![Value::Lit(name_lit), Value::Var(encoded)],
                span: zero_span(),
            });
            let list = ctx.fresh_var();
            stmts.push(Stmt::ListNew {
                dst: list,
                items: vec![Value::Var(pair)],
                span: zero_span(),
            });
            pair_lists.push(Value::Var(list));
        }
    }
    let lists = ctx.fresh_var();
    stmts.push(Stmt::ListNew {
        dst: lists,
        items: pair_lists,
        span: zero_span(),
    });
    let flat = ctx.fresh_var();
    stmts.push(Stmt::CallRt {
        dst: Some(flat),
        callee: RuntimeFn::LFlatten,
        args: vec![Value::Var(lists)],
        live_after: LiveAfter::default(),
        span: zero_span(),
    });
    Ok(Block {
        stmts,
        tail: Tail::TailRt {
            callee: RuntimeFn::JObject,
            args: vec![Value::Var(flat)],
        },
    })
}

/// One optional field's `case value { Some(inner) -> [#(name, json)]
/// None -> [] }` lifted pair function.
fn pair_fn(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    field: &FieldShape,
    field_index: usize,
    host: FnRef,
    position: u32,
) -> Result<MirFn, LowerError> {
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
    ctx.reset_vars();
    let value = ctx.fresh_var();
    let some_atom = ctx.atom("some");
    let mut then_stmts = Vec::new();
    let unwrapped = ctx.fresh_var();
    then_stmts.push(Stmt::FieldGet {
        dst: unwrapped,
        base: Value::Var(value),
        index: 1,
        span: zero_span(),
    });
    let encoded = encode_value(ctx, plan, &inner, Value::Var(unwrapped), &mut then_stmts)?;
    let name_lit = ctx.binary(&field.awl_name);
    let pair = ctx.fresh_var();
    then_stmts.push(Stmt::TupleNew {
        dst: pair,
        items: vec![Value::Lit(name_lit), Value::Var(encoded)],
        span: zero_span(),
    });
    let list = ctx.fresh_var();
    then_stmts.push(Stmt::ListNew {
        dst: list,
        items: vec![Value::Var(pair)],
        span: zero_span(),
    });
    let body = Block {
        stmts: Vec::new(),
        tail: Tail::If {
            test: Test::IsTagged {
                value: Value::Var(value),
                tag: some_atom,
                arity: 2,
            },
            then_block: Box::new(Block {
                stmts: then_stmts,
                tail: Tail::Return(Value::Var(list)),
            }),
            else_block: Box::new(Block {
                stmts: Vec::new(),
                tail: Tail::Return(Value::Nil),
            }),
            span: zero_span(),
        },
    };
    let field_ty = desc_tydesc(ctx, &field.desc);
    Ok(MirFn::Flow(FlowFn {
        origin: lifted_origin(host, position),
        name: format!("{}_to_json$field{field_index}", codec_type.stem),
        params: vec![value],
        param_tys: vec![field_ty],
        ret_ty: TyDesc::List(Box::new(TyDesc::Tuple(vec![TyDesc::String, TyDesc::Json]))),
        body,
        span: zero_span(),
        degraded_parallel: false,
    }))
}

/// Enum `_to_json`: `case value { Variant -> json.string("Variant") … }`.
pub(super) fn enum_shape(
    ctx: &mut Ctx<'_>,
    codec_type: &CodecType,
    shape: &TypeShape,
    params: TrioParams,
) -> Stamped {
    let variants = match shape {
        TypeShape::Enum { variants, .. } => variants.clone(),
        _ => Vec::new(),
    };
    ctx.reset_vars();
    let subject = ctx.fresh_var();
    let arms = variants
        .iter()
        .map(|(ctor, json_name)| {
            let lit = ctx.binary(json_name);
            (
                *ctor,
                Block {
                    stmts: Vec::new(),
                    tail: Tail::TailRt {
                        callee: RuntimeFn::JString,
                        args: vec![Value::Lit(lit)],
                    },
                },
            )
        })
        .collect();
    let body = Block {
        stmts: Vec::new(),
        tail: Tail::SelectEnum {
            subject: Value::Var(subject),
            arms,
            span: zero_span(),
        },
    };
    Stamped {
        main: flow(codec_type, params, subject, body),
        lifted: Vec::new(),
    }
}

/// Union `_to_json`: `{outcome, payload}` per arm, an exhaustive tagged-test
/// chain (the last arm is the final else — Gleam case exhaustiveness).
pub(super) fn union(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    shape: &TypeShape,
    params: TrioParams,
) -> Result<Stamped, LowerError> {
    let TypeShape::Union { arms, .. } = shape else {
        return Err(LowerError::new(
            ctx.emitter.document.span,
            format!("codec `{}` is not a union shape", codec_type.stem),
        ));
    };
    let arms = arms.clone();
    if arms.is_empty() {
        return Err(LowerError::new(
            ctx.emitter.document.span,
            "the outcome union has no success arms",
        ));
    }
    ctx.reset_vars();
    let subject = ctx.fresh_var();
    // Build arm blocks in reverse: each earlier arm's else is the rest.
    let mut tail: Option<Block> = None;
    for arm in arms.iter().rev() {
        let mut stmts = Vec::new();
        let payload = ctx.fresh_var();
        stmts.push(Stmt::FieldGet {
            dst: payload,
            base: Value::Var(subject),
            index: 1,
            span: zero_span(),
        });
        let outcome_lit = ctx.binary(&arm.outcome);
        let object = ctx.fresh_var();
        stmts.push(Stmt::JsonObj {
            dst: object,
            pairs: vec![
                (
                    "outcome".to_owned(),
                    JsonVal::Encoded {
                        value: Value::Lit(outcome_lit),
                        via: super::super::ops::ToJsonRef::SdkLeaf(super::super::tydesc::Leaf::Str),
                    },
                ),
                (
                    "payload".to_owned(),
                    JsonVal::Encoded {
                        value: Value::Var(payload),
                        via: to_json_ref_for(ctx, plan, &arm.payload)?,
                    },
                ),
            ],
            span: zero_span(),
        });
        let arm_block = Block {
            stmts,
            tail: Tail::Return(Value::Var(object)),
        };
        tail = Some(match tail {
            // The innermost (last) arm needs no test: the case is exhaustive.
            None => arm_block,
            Some(rest) => Block {
                stmts: Vec::new(),
                tail: Tail::If {
                    test: Test::IsTagged {
                        value: Value::Var(subject),
                        tag: arm.ctor,
                        arity: 2,
                    },
                    then_block: Box::new(arm_block),
                    else_block: Box::new(rest),
                    span: zero_span(),
                },
            },
        });
    }
    let body = tail.unwrap_or(Block {
        stmts: Vec::new(),
        tail: Tail::Return(Value::Nil),
    });
    Ok(Stamped {
        main: flow(codec_type, params, subject, body),
        lifted: Vec::new(),
    })
}

/// Composite `_to_json`: `json.array(values, inner)` / `json.nullable(value,
/// inner)`, the inner passed as a fun value (a lifted wrapper for leaves, the
/// local trio fn otherwise).
pub(super) fn composite(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    codec_type: &CodecType,
    desc: &WireDesc,
    params: TrioParams,
    lifted_refs: &[FnRef],
) -> Result<Stamped, LowerError> {
    let (callee, inner) = match desc {
        WireDesc::List(inner) => (RuntimeFn::JArray, (**inner).clone()),
        WireDesc::Nullable(inner) => (RuntimeFn::JNullable, (**inner).clone()),
        _ => {
            return Err(LowerError::new(
                ctx.emitter.document.span,
                format!("composite codec `{}` is not a list/option", codec_type.stem),
            ));
        }
    };
    ctx.reset_vars();
    let subject = ctx.fresh_var();
    let mut stmts = Vec::new();
    let mut lifted = Vec::new();
    let (_, to_json_ref, _) = plan.codecs[&codec_type.stem];
    let fun = ctx.fresh_var();
    if let Some(leaf) = leaf_of_desc(&inner) {
        let item_ref = lifted_refs.first().copied().ok_or_else(|| {
            LowerError::new(
                ctx.emitter.document.span,
                format!("composite codec `{}` has no item-fn slot", codec_type.stem),
            )
        })?;
        stmts.push(Stmt::MakeClosure {
            dst: fun,
            lifted: item_ref,
            captures: Vec::new(),
            span: zero_span(),
        });
        lifted.push(leaf_item_fn(ctx, codec_type, &inner, leaf, to_json_ref));
    } else {
        let inner_to_json = super::codec::desc_trio(ctx, plan, &inner)?.1;
        stmts.push(Stmt::MakeClosure {
            dst: fun,
            lifted: inner_to_json,
            captures: Vec::new(),
            span: zero_span(),
        });
    }
    let body = Block {
        stmts,
        tail: Tail::TailRt {
            callee,
            args: vec![Value::Var(subject), Value::Var(fun)],
        },
    };
    Ok(Stamped {
        main: flow(codec_type, params, subject, body),
        lifted,
    })
}

/// The leaf `to_json` fun-value wrapper a composite's inner leaf rides
/// (`awlc.<leaf>_to_json` is an import, so the fun value needs a local body).
fn leaf_item_fn(
    ctx: &mut Ctx<'_>,
    codec_type: &CodecType,
    inner: &WireDesc,
    leaf: super::super::tydesc::Leaf,
    host: FnRef,
) -> MirFn {
    ctx.reset_vars();
    let value = ctx.fresh_var();
    let inner_ty = desc_tydesc(ctx, inner);
    MirFn::Flow(FlowFn {
        origin: lifted_origin(host, 0),
        name: format!("{}_to_json$item", codec_type.stem),
        params: vec![value],
        param_tys: vec![inner_ty],
        ret_ty: TyDesc::Json,
        body: Block {
            stmts: Vec::new(),
            tail: Tail::TailRt {
                callee: RuntimeFn::LeafToJson(leaf),
                args: vec![Value::Var(value)],
            },
        },
        span: zero_span(),
        degraded_parallel: false,
    })
}
