//! Direct MIR lowering for quantified collection predicates.

use std::collections::BTreeSet;

use crate::ast::{Expr, Quantifier};
use crate::emitter::GType;

use super::super::func::{FlowFn, FnOrigin, MirFn};
use super::super::ids::{FnRef, Span, Var};
use super::super::ops::{Block, LiveAfter, Stmt, Tail, Test, Value};
use super::super::runtime::RuntimeFn;
use super::super::tydesc::TyDesc;
use super::activity::call_rt;
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope, lower_expr};

const ACCESSOR: &str = "\0awl_predicate_item";

fn span_of(span: crate::Span) -> Span {
    Span::from_source(span)
}

pub(super) fn lower_accessor(
    ctx: &mut Ctx<'_>,
    span: crate::Span,
    name: &str,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(Value, GType), LowerError> {
    let item = scope
        .get(ACCESSOR)
        .ok_or_else(|| LowerError::new(span, "a `.field` accessor needs a collection predicate"))?;
    let (index, ty) = ctx.field_index(&item.ty, name, span)?;
    let dst = ctx.fresh_var();
    stmts.push(Stmt::FieldGet {
        dst,
        base: Value::Var(item.var),
        index,
        span: span_of(span),
    });
    Ok((Value::Var(dst), ty))
}

pub(super) fn lower_collection_predicate(
    ctx: &mut Ctx<'_>,
    collection: &Expr,
    quantifier: Quantifier,
    predicate: &Expr,
    span: crate::Span,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(Value, GType), LowerError> {
    let (items, collection_ty) = lower_expr(ctx, collection, scope, stmts)?;
    let GType::List(element) = ctx.emitter.env.resolve(&collection_ty) else {
        return Err(LowerError::new(span, "collection predicate needs a list"));
    };
    let mut referenced = BTreeSet::new();
    crate::emitter::expr_refs(predicate, &mut referenced);
    let captures: Vec<String> = referenced
        .into_iter()
        .filter(|name| scope.contains_key(name))
        .collect();
    let (ordinal, predicate_ref) = ctx.take_predicate()?;
    let fallible = crate::emitter::predicate_is_fallible(predicate);
    let saved = ctx.swap_var_counter(0);
    let built = build_predicate_fn(
        ctx,
        PredicateBuild {
            ordinal,
            predicate,
            quantifier,
            fallible,
            element: &element,
            captures: &captures,
            host_scope: scope,
            span,
        },
    );
    ctx.swap_var_counter(saved);
    ctx.finish_predicate(ordinal, MirFn::Flow(built?));

    let host_captures = captures
        .iter()
        .filter_map(|name| scope.get(name).map(|binding| Value::Var(binding.var)))
        .collect();
    let closure = ctx.fresh_var();
    stmts.push(Stmt::MakeClosure {
        dst: closure,
        lifted: predicate_ref,
        captures: host_captures,
        span: span_of(span),
    });
    let runtime = match (quantifier, fallible) {
        (_, true) => RuntimeFn::LTryFold,
        (Quantifier::Any, false) => RuntimeFn::LAny,
        (Quantifier::All, false) => RuntimeFn::LAll,
    };
    let mut args = vec![items];
    if fallible {
        args.push(Value::Atom(ctx.atom(match quantifier {
            Quantifier::Any => "false",
            Quantifier::All => "true",
        })));
    }
    args.push(Value::Var(closure));
    let result = call_rt(ctx, runtime, args, stmts, span);
    if !fallible {
        return Ok((Value::Var(result), GType::Bool));
    }
    let dst = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst,
        result,
        live_after: LiveAfter::default(),
        span: span_of(span),
    });
    Ok((Value::Var(dst), GType::Bool))
}

#[derive(Clone, Copy)]
struct PredicateBuild<'a> {
    ordinal: usize,
    predicate: &'a Expr,
    quantifier: Quantifier,
    fallible: bool,
    element: &'a GType,
    captures: &'a [String],
    host_scope: &'a Scope,
    span: crate::Span,
}

fn build_predicate_fn(ctx: &mut Ctx<'_>, build: PredicateBuild<'_>) -> Result<FlowFn, LowerError> {
    let PredicateBuild {
        ordinal,
        predicate,
        quantifier,
        fallible,
        element,
        captures,
        host_scope,
        span,
    } = build;
    let accumulator = fallible.then(|| ctx.fresh_var());
    let item = ctx.fresh_var();
    let mut params = accumulator.into_iter().chain([item]).collect::<Vec<_>>();
    let mut param_tys = if fallible {
        vec![TyDesc::Bool, ctx.tydesc(element)]
    } else {
        vec![ctx.tydesc(element)]
    };
    let mut scope = Scope::new();
    scope.insert(
        ACCESSOR.to_owned(),
        Binding {
            var: item,
            ty: element.clone(),
        },
    );
    add_capture_params(
        ctx,
        captures,
        host_scope,
        span,
        &mut params,
        &mut param_tys,
        &mut scope,
    )?;
    let mut stmts = Vec::new();
    let (value, _) = lower_expr(ctx, predicate, &scope, &mut stmts)?;
    let body = predicate_body(ctx, quantifier, fallible, accumulator, value, stmts, span);
    Ok(FlowFn {
        origin: FnOrigin::LiftedClosure {
            host: FnRef(2),
            index: u32::try_from(ordinal).map_or(u32::MAX, |index| index),
        },
        name: format!("awl_predicate_{ordinal}"),
        params,
        param_tys,
        ret_ty: if fallible {
            TyDesc::Result(Box::new(TyDesc::Bool), Box::new(TyDesc::AwlError))
        } else {
            TyDesc::Bool
        },
        body,
        span: span_of(span),
        degraded_parallel: false,
    })
}

fn predicate_body(
    ctx: &mut Ctx<'_>,
    quantifier: Quantifier,
    fallible: bool,
    accumulator: Option<Var>,
    value: Value,
    stmts: Vec<Stmt>,
    span: crate::Span,
) -> Block {
    let Some(accumulator) = accumulator.filter(|_| fallible) else {
        return Block {
            stmts,
            tail: Tail::Return(value),
        };
    };
    let evaluated = ok_block(ctx, value, stmts, span);
    let decisive = ok_block(ctx, Value::Var(accumulator), Vec::new(), span);
    let test = Test::IsTrue(Value::Var(accumulator));
    let (then_block, else_block) = match quantifier {
        Quantifier::Any => (decisive, evaluated),
        Quantifier::All => (evaluated, decisive),
    };
    Block {
        stmts: Vec::new(),
        tail: Tail::If {
            test,
            then_block: Box::new(then_block),
            else_block: Box::new(else_block),
            span: span_of(span),
        },
    }
}

fn ok_block(ctx: &mut Ctx<'_>, value: Value, mut stmts: Vec<Stmt>, span: crate::Span) -> Block {
    let result = ctx.fresh_var();
    stmts.push(Stmt::RecordNew {
        dst: result,
        tag: ctx.atom("ok"),
        args: vec![value],
        span: span_of(span),
    });
    Block {
        stmts,
        tail: Tail::Return(Value::Var(result)),
    }
}

fn add_capture_params(
    ctx: &mut Ctx<'_>,
    captures: &[String],
    host_scope: &Scope,
    span: crate::Span,
    params: &mut Vec<Var>,
    param_tys: &mut Vec<TyDesc>,
    scope: &mut Scope,
) -> Result<(), LowerError> {
    for name in captures {
        let host = host_scope.get(name).ok_or_else(|| {
            LowerError::new(span, format!("predicate capture `{name}` is not in scope"))
        })?;
        let var = ctx.fresh_var();
        params.push(var);
        param_tys.push(ctx.tydesc(&host.ty));
        scope.insert(
            name.clone(),
            Binding {
                var,
                ty: host.ty.clone(),
            },
        );
    }
    Ok(())
}
