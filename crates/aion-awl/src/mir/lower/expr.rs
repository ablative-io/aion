//! Keel-expression lowering for the BC-2 covered subset: refs, field access,
//! literals, variants, record construction, general string concatenation, and
//! the boolean/comparison forms used by outcome guards (`exprs.rs`). Deferred forms return an explicit
//! `LowerError::unsupported` — visible incompleteness, never silent drift.

use std::collections::BTreeMap;

use crate::ast::{Arg, BinaryOp, Expr, PredicateKind};
use crate::emitter::{GType, snake};

use super::super::ids::{Span, Var};
use super::super::ops::{BoolBin, CmpOp, LiveAfter, Stmt, Value};
use super::super::runtime::RuntimeFn;
use super::activity::call_rt;
use super::collection_predicate::{lower_accessor, lower_collection_predicate};
use super::ctx::Ctx;
use super::driver::LowerError;

/// A bound name with its type, for field-index and optional-wrap resolution.
#[derive(Clone)]
pub(super) struct Binding {
    pub(super) var: Var,
    pub(super) ty: GType,
}

pub(super) type Scope = BTreeMap<String, Binding>;

fn span_of(span: crate::Span) -> Span {
    Span::from_source(span)
}

/// Lower an expression to a value, pushing any prelude statements. Returns the
/// value and its `GType`.
pub(super) fn lower_expr(
    ctx: &mut Ctx<'_>,
    expr: &Expr,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(Value, GType), LowerError> {
    match expr {
        Expr::String { value, span } => {
            let lit = ctx.binary(value);
            let _ = span;
            Ok((Value::Lit(lit), GType::Str))
        }
        Expr::Int { value, span } => {
            let signed = i64::try_from(*value)
                .map_err(|_| LowerError::unsupported("integer literal above i64::MAX", *span))?;
            Ok((Value::Int(signed), GType::Int))
        }
        Expr::Bool { value, .. } => {
            let atom = ctx.atom(if *value { "true" } else { "false" });
            Ok((Value::Atom(atom), GType::Bool))
        }
        Expr::Float { value, span } => {
            let lit = ctx.float_literal(value);
            let _ = span;
            Ok((Value::Lit(lit), GType::Float))
        }
        Expr::List { items, span } => lower_list_literal(ctx, items, *span, stmts),
        Expr::Variant { name, .. } => {
            let atom = ctx.atom(&snake(name));
            let ty = ctx.variant_enum(name).unwrap_or(GType::Unknown);
            Ok((Value::Atom(atom), ty))
        }
        Expr::Ref { name, span } => {
            let binding = scope.get(name).ok_or_else(|| {
                LowerError::new(*span, format!("`{name}` has no binding in scope"))
            })?;
            Ok((Value::Var(binding.var), binding.ty.clone()))
        }
        Expr::Workflow { span } => Err(LowerError::new(
            *span,
            "`workflow` is a namespace, not a value",
        )),
        Expr::Field {
            base,
            name,
            name_span,
            ..
        } => {
            if matches!(base.as_ref(), Expr::Workflow { .. }) && name == "id" {
                let result = call_rt(ctx, RuntimeFn::WfId, Vec::new(), stmts, *name_span);
                let mapped = call_rt(
                    ctx,
                    RuntimeFn::MapEngineError,
                    vec![Value::Var(result)],
                    stmts,
                    *name_span,
                );
                let dst = ctx.fresh_var();
                stmts.push(Stmt::TryBind {
                    dst,
                    result: mapped,
                    live_after: LiveAfter::default(),
                    span: span_of(*name_span),
                });
                return Ok((Value::Var(dst), GType::Str));
            }
            let (base_value, base_ty) = lower_expr(ctx, base, scope, stmts)?;
            let (index, field_ty) = ctx.field_index(&base_ty, name, *name_span)?;
            let dst = ctx.fresh_var();
            stmts.push(Stmt::FieldGet {
                dst,
                base: base_value,
                index,
                span: span_of(*name_span),
            });
            Ok((Value::Var(dst), field_ty))
        }
        Expr::Record {
            name,
            name_span,
            args,
            span,
        } => lower_record(ctx, name, *name_span, args, scope, stmts, *span),
        Expr::Accessor { span, name } => lower_accessor(ctx, *span, name, scope, stmts),
        Expr::CollectionPredicate {
            collection,
            quantifier,
            predicate,
            span,
        } => {
            lower_collection_predicate(ctx, collection, *quantifier, predicate, *span, scope, stmts)
        }
        Expr::Not { .. } | Expr::Binary { .. } | Expr::Predicate { .. } => {
            lower_logic(ctx, expr, scope, stmts)
        }
        other => Err(LowerError::unsupported("expression", expr_span(other))),
    }
}

fn lower_list_literal(
    ctx: &mut Ctx<'_>,
    items: &[Expr],
    span: crate::Span,
    stmts: &mut Vec<Stmt>,
) -> Result<(Value, GType), LowerError> {
    if !items.is_empty() {
        return Err(LowerError::unsupported("non-empty list literal", span));
    }
    let dst = ctx.fresh_var();
    stmts.push(Stmt::ListNew {
        dst,
        items: Vec::new(),
        span: span_of(span),
    });
    Ok((Value::Var(dst), GType::List(Box::new(GType::Unknown))))
}

fn lower_logic(
    ctx: &mut Ctx<'_>,
    expr: &Expr,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(Value, GType), LowerError> {
    let dst = ctx.fresh_var();
    match expr {
        Expr::Not { expr, span } => {
            let (src, _) = lower_expr(ctx, expr, scope, stmts)?;
            stmts.push(Stmt::Not {
                dst,
                src,
                span: span_of(*span),
            });
        }
        Expr::Binary {
            left,
            op,
            right,
            span,
        } => {
            let (lhs, lhs_ty) = lower_expr(ctx, left, scope, stmts)?;
            let (rhs, rhs_ty) = lower_expr(ctx, right, scope, stmts)?;
            match op {
                BinaryOp::And | BinaryOp::Or => stmts.push(Stmt::BoolOp {
                    dst,
                    op: if matches!(op, BinaryOp::And) {
                        BoolBin::And
                    } else {
                        BoolBin::Or
                    },
                    lhs,
                    rhs,
                    span: span_of(*span),
                }),
                BinaryOp::Concat => {
                    let resolved = (
                        ctx.emitter.env.resolve(&lhs_ty),
                        ctx.emitter.env.resolve(&rhs_ty),
                    );
                    if !matches!(resolved, (GType::Str, GType::Str)) {
                        return Err(LowerError::unsupported("string concatenation", *span));
                    }
                    stmts.push(Stmt::Concat {
                        dst,
                        lhs,
                        rhs,
                        span: span_of(*span),
                    });
                }
                comparison => stmts.push(Stmt::Cmp {
                    dst,
                    op: cmp_op(*comparison, &lhs_ty),
                    lhs,
                    rhs,
                    span: span_of(*span),
                }),
            }
        }
        Expr::Predicate {
            subject,
            kind,
            span,
        } => {
            let (lhs, _) = lower_expr(ctx, subject, scope, stmts)?;
            let rhs = match kind {
                PredicateKind::Empty => Value::Nil,
                PredicateKind::Present | PredicateKind::Absent => Value::Atom(ctx.atom("none")),
            };
            stmts.push(Stmt::Cmp {
                dst,
                op: if matches!(kind, PredicateKind::Present) {
                    CmpOp::Ne
                } else {
                    CmpOp::Eq
                },
                lhs,
                rhs,
                span: span_of(*span),
            });
        }
        _ => unreachable!("lower_logic called for a non-logical expression"),
    }
    let ty = if matches!(
        expr,
        Expr::Binary {
            op: BinaryOp::Concat,
            ..
        }
    ) {
        GType::Str
    } else {
        GType::Bool
    };
    Ok((Value::Var(dst), ty))
}

fn cmp_op(op: BinaryOp, lhs_ty: &GType) -> CmpOp {
    let float = matches!(lhs_ty, GType::Float);
    match (op, float) {
        (BinaryOp::Eq, _) => CmpOp::Eq,
        (BinaryOp::Ne, _) => CmpOp::Ne,
        (BinaryOp::Lt, false) => CmpOp::Lt,
        (BinaryOp::Le, false) => CmpOp::Le,
        (BinaryOp::Gt, false) => CmpOp::Gt,
        (BinaryOp::Ge, false) => CmpOp::Ge,
        (BinaryOp::Lt, true) => CmpOp::FLt,
        (BinaryOp::Le, true) => CmpOp::FLe,
        (BinaryOp::Gt, true) => CmpOp::FGt,
        (BinaryOp::Ge, true) => CmpOp::FGe,
        (BinaryOp::And | BinaryOp::Or | BinaryOp::Concat, _) => unreachable!(),
    }
}

/// Lower a value for a typed slot, wrapping a present value in `Some` when the
/// slot is optional and the value is not (`render_arg_for`).
pub(super) fn lower_arg_for(
    ctx: &mut Ctx<'_>,
    expr: &Expr,
    expected: &GType,
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<Value, LowerError> {
    let (value, actual) = lower_expr(ctx, expr, scope, stmts)?;
    Ok(wrap_optional_value(
        ctx,
        value,
        &actual,
        expected,
        stmts,
        expr_span(expr),
    ))
}

/// Wrap a present value in `Some` (`{some, V}`) when the slot is `Option` and
/// the value is not — the reference `wrap_optional` (`pipes.rs:203`). Shared by
/// argument lowering and the pipe-into-action path so both call forms of the
/// same action produce identical terms.
pub(super) fn wrap_optional_value(
    ctx: &mut Ctx<'_>,
    value: Value,
    actual: &GType,
    expected: &GType,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Value {
    if matches!(ctx.emitter.env.resolve(expected), GType::Option(_))
        && !matches!(ctx.emitter.env.resolve(actual), GType::Option(_))
    {
        let some = ctx.atom("some");
        let dst = ctx.fresh_var();
        stmts.push(Stmt::RecordNew {
            dst,
            tag: some,
            args: vec![value],
            span: span_of(span),
        });
        return Value::Var(dst);
    }
    value
}

fn lower_record(
    ctx: &mut Ctx<'_>,
    name: &str,
    name_span: crate::Span,
    args: &[Arg],
    scope: &Scope,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Result<(Value, GType), LowerError> {
    let ty = GType::Named(name.to_owned());
    let Some((gleam_name, record)) = ctx.emitter.env.record_of(&ty) else {
        return Err(LowerError::new(
            name_span,
            format!("`{name}` is not a constructible record"),
        ));
    };
    let fields = record.fields.clone();
    let tag = ctx.atom(&snake(&gleam_name));
    if fields.is_empty() {
        return Ok((Value::Atom(tag), ty));
    }
    let mut values = Vec::new();
    for field in &fields {
        let value = match args.iter().find(|arg| arg.name == field.awl_name) {
            Some(arg) => lower_arg_for(ctx, &arg.value, &field.ty, scope, stmts)?,
            None if matches!(ctx.emitter.env.resolve(&field.ty), GType::Option(_)) => {
                Value::Atom(ctx.atom("none"))
            }
            None => {
                return Err(LowerError::new(
                    span,
                    format!("constructing `{name}` misses field `{}`", field.awl_name),
                ));
            }
        };
        values.push(value);
    }
    let dst = ctx.fresh_var();
    stmts.push(Stmt::RecordNew {
        dst,
        tag,
        args: values,
        span: span_of(span),
    });
    Ok((Value::Var(dst), ty))
}

fn expr_span(expr: &Expr) -> crate::Span {
    match expr {
        Expr::String { span, .. }
        | Expr::Int { span, .. }
        | Expr::Float { span, .. }
        | Expr::Bool { span, .. }
        | Expr::Ref { span, .. }
        | Expr::Workflow { span }
        | Expr::Variant { span, .. }
        | Expr::Record { span, .. }
        | Expr::Field { span, .. }
        | Expr::Index { span, .. }
        | Expr::Accessor { span, .. }
        | Expr::Not { span, .. }
        | Expr::Binary { span, .. }
        | Expr::List { span, .. }
        | Expr::Predicate { span, .. }
        | Expr::CollectionPredicate { span, .. } => *span,
        Expr::Duration(duration) => duration.span,
    }
}

impl Ctx<'_> {
    fn variant_enum(&self, variant: &str) -> Option<GType> {
        for candidate in &self.emitter.env.order {
            if let Some(crate::emitter::NamedDef::Enum(variants)) = self.emitter.env.get(candidate)
                && variants.iter().any(|name| name == variant)
            {
                return Some(GType::Named(candidate.clone()));
            }
        }
        None
    }

    /// The 1-based tuple element index and type of a record field.
    pub(super) fn field_index(
        &self,
        base_ty: &GType,
        field: &str,
        span: crate::Span,
    ) -> Result<(u16, GType), LowerError> {
        let (_, record) = self
            .emitter
            .env
            .record_of(base_ty)
            .ok_or_else(|| LowerError::new(span, format!("`.{field}` needs a record type")))?;
        let position = record
            .fields
            .iter()
            .position(|candidate| candidate.awl_name == field)
            .ok_or_else(|| LowerError::new(span, format!("no field `{field}`")))?;
        let ty = record.fields[position].ty.clone();
        Ok((u16::try_from(position + 1).unwrap_or(u16::MAX), ty))
    }

    fn float_literal(&mut self, lexeme: &str) -> super::super::ids::LitRef {
        self.push_float(lexeme)
    }
}
