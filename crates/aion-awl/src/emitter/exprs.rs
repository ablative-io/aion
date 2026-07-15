//! Expression typing and rendering.
//!
//! Rendering may need *prelude* lines: literal indexing (`items[0]`) is a
//! fallible operation, so each `Index` node lowers to a fresh `use … <- try(
//! awl_index(…))` line emitted before the expression's use site, and the
//! expression renders as that fresh name. Contexts that cannot host a
//! prelude (outcome guards) refuse indexing with a spanned error.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::{Deref, DerefMut};

use crate::ast::{Arg, BinaryOp, Expr, PredicateKind};
use crate::spanned::Spanned;
use crate::{DurationUnit, Span};

use super::context::Emitter;
use super::error::EmitError;
use super::names::{ident, string_lit};
use super::types::{GType, NamedDef};

/// Bindings in scope during rendering, with their types.
#[derive(Clone, Default)]
pub(super) struct Scope {
    vars: BTreeMap<String, GType>,
    accessor: Option<(String, GType)>,
}

impl Scope {
    pub(super) const fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
            accessor: None,
        }
    }

    pub(super) fn from_vars(vars: BTreeMap<String, GType>) -> Self {
        Self {
            vars,
            accessor: None,
        }
    }

    pub(super) fn with_accessor(&self, name: String, ty: GType) -> Self {
        let mut nested = self.clone();
        nested.accessor = Some((name, ty));
        nested
    }
}

impl Deref for Scope {
    type Target = BTreeMap<String, GType>;

    fn deref(&self) -> &Self::Target {
        &self.vars
    }
}

impl DerefMut for Scope {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.vars
    }
}

pub(super) fn duration_ms(duration: &crate::ast::DurationLiteral) -> u64 {
    match duration.unit {
        DurationUnit::Seconds => duration.magnitude.saturating_mul(1_000),
        DurationUnit::Minutes => duration.magnitude.saturating_mul(60_000),
        DurationUnit::Hours => duration.magnitude.saturating_mul(3_600_000),
        DurationUnit::Days => duration.magnitude.saturating_mul(86_400_000),
    }
}

pub(super) fn duration_expr(duration: &crate::ast::DurationLiteral) -> String {
    format!("duration.milliseconds({})", duration_ms(duration))
}

/// Infer the type of an expression against the current scope.
pub(super) fn expr_type(
    emitter: &Emitter<'_>,
    expr: &Expr,
    scope: &Scope,
) -> Result<GType, EmitError> {
    match expr {
        Expr::String { .. }
        | Expr::RawString { .. }
        | Expr::Json { .. }
        | Expr::SchemaOf { .. } => Ok(GType::Str),
        Expr::Int { .. } => Ok(GType::Int),
        Expr::Float { .. } => Ok(GType::Float),
        Expr::Bool { .. }
        | Expr::Not { .. }
        | Expr::Predicate { .. }
        | Expr::CollectionPredicate { .. } => Ok(GType::Bool),
        Expr::Duration(_) => Ok(GType::Duration),
        Expr::List { items, .. } => match items.first() {
            Some(first) => Ok(GType::List(Box::new(expr_type(emitter, first, scope)?))),
            None => Ok(GType::List(Box::new(GType::Unknown))),
        },
        Expr::Ref { span, name } => scope.get(name).cloned().ok_or_else(|| {
            EmitError::new(
                *span,
                format!("`{name}` has no binding with a known type in scope"),
            )
        }),
        Expr::Workflow { span } => Err(EmitError::new(
            *span,
            "`workflow` is a namespace, not a value",
        )),
        Expr::Variant { span, name } => {
            for candidate in &emitter.env.order {
                if let Some(NamedDef::Enum(variants)) = emitter.env.get(candidate)
                    && variants.iter().any(|variant| variant == name)
                {
                    return Ok(GType::Named(candidate.clone()));
                }
            }
            Err(EmitError::new(
                *span,
                format!("`{name}` is not a variant of any declared enum"),
            ))
        }
        Expr::Record { name, .. } => Ok(GType::Named(name.clone())),
        Expr::Field {
            base,
            name,
            name_span,
            ..
        } => {
            if matches!(base.as_ref(), Expr::Workflow { .. }) && name == "id" {
                return Ok(GType::Str);
            }
            let base_ty = expr_type(emitter, base, scope)?;
            field_type(emitter, &base_ty, name, *name_span)
        }
        Expr::Index { span, base, .. } => {
            let base_ty = emitter.env.resolve(&expr_type(emitter, base, scope)?);
            match base_ty {
                GType::List(inner) => Ok(*inner),
                other => Err(EmitError::new(
                    *span,
                    format!(
                        "indexing needs a list, found {}",
                        emitter.env.gleam_type(&other)
                    ),
                )),
            }
        }
        Expr::Accessor { span, name } => match &scope.accessor {
            Some((_, element)) => field_type(emitter, element, name, *span),
            None => Err(EmitError::new(
                *span,
                "a bare `.field` accessor is only meaningful as a combinator argument",
            )),
        },
        Expr::Binary { op, .. } => Ok(match op {
            BinaryOp::Concat => GType::Str,
            _ => GType::Bool,
        }),
    }
}

/// Resolve a field access against the environment.
pub(super) fn field_type(
    emitter: &Emitter<'_>,
    base_ty: &GType,
    field: &str,
    span: Span,
) -> Result<GType, EmitError> {
    match emitter.env.record_of(base_ty) {
        Some((_, record)) => record
            .fields
            .iter()
            .find(|candidate| candidate.awl_name == field)
            .map(|candidate| candidate.ty.clone())
            .ok_or_else(|| {
                EmitError::new(
                    span,
                    format!("no field `{field}` on {}", emitter.env.gleam_type(base_ty)),
                )
            }),
        None => match emitter.env.resolve(base_ty) {
            GType::Option(_) => Err(EmitError::new(
                span,
                format!("`.{field}` reads an optional value — guard it with `is present` first"),
            )),
            other => Err(EmitError::new(
                span,
                format!(
                    "`.{field}` needs a record, found {}",
                    emitter.env.gleam_type(&other)
                ),
            )),
        },
    }
}

/// Render an expression; fallible indexing pushes `use … <- try(…)` lines
/// onto `prelude` (emit them before using the returned text).
pub(super) fn render_expr(
    emitter: &mut Emitter<'_>,
    expr: &Expr,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    match expr {
        Expr::String { value, .. } | Expr::RawString { value, .. } => Ok(string_lit(value)),
        Expr::Json { body, .. } => Ok(string_lit(body)),
        Expr::SchemaOf { span, .. } => Err(EmitError::new(
            *span,
            "`schema of` must be folded before emission — this is an emitter defect",
        )),
        Expr::Int { value, .. } => Ok(value.to_string()),
        Expr::Float { value, .. } => Ok(value.clone()),
        Expr::Bool { value, .. } => Ok(if *value { "True" } else { "False" }.to_owned()),
        Expr::Duration(duration) => Ok(duration_expr(duration)),
        Expr::List { items, .. } => {
            let rendered = items
                .iter()
                .map(|item| render_expr(emitter, item, scope, prelude))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            Ok(format!("[{rendered}]"))
        }
        Expr::Ref { name, .. } => Ok(ident(name)),
        Expr::Workflow { span } => Err(EmitError::new(
            *span,
            "`workflow` is a namespace, not a value",
        )),
        Expr::Variant { name, .. } => Ok(name.clone()),
        Expr::Record {
            span,
            name,
            name_span,
            args,
        } => render_record(emitter, *span, name, *name_span, args, scope, prelude),
        Expr::Field { base, name, .. } => {
            if matches!(base.as_ref(), Expr::Workflow { .. }) && name == "id" {
                let fresh = format!("awl_workflow_id_{}", prelude.len());
                prelude.push(format!(
                    "use {fresh} <- result.try(workflow.id() |> awl_error.map_engine_error)"
                ));
                return Ok(fresh);
            }
            let base = render_expr(emitter, base, scope, prelude)?;
            Ok(format!("{base}.{}", ident(name)))
        }
        Expr::Index {
            span, base, index, ..
        } => {
            let base_rendered = render_expr(emitter, base, scope, prelude)?;
            let fresh = format!("awl_index_{}", prelude.len());
            prelude.push(format!(
                "use {fresh} <- result.try(runtime.index({base_rendered}, {index}, \
                 \"index {index} out of range at line {}, column {}\"))",
                span.line, span.column
            ));
            Ok(fresh)
        }
        Expr::Accessor { span, name } => match &scope.accessor {
            Some((item, _)) => Ok(format!("{}.{}", ident(item), ident(name))),
            None => Err(EmitError::new(
                *span,
                "a bare `.field` accessor is only meaningful as a combinator argument",
            )),
        },
        Expr::Not { expr: inner, .. } => {
            let inner = render_parenthesized(emitter, inner, scope, prelude)?;
            Ok(format!("!{inner}"))
        }
        Expr::Binary {
            left, op, right, ..
        } => render_binary(emitter, left, *op, right, scope, prelude),
        Expr::Predicate { subject, kind, .. } => {
            let subject = render_parenthesized(emitter, subject, scope, prelude)?;
            Ok(match kind {
                PredicateKind::Empty => {
                    emitter.flags.uses_list_module = true;
                    format!("list.is_empty({subject})")
                }
                PredicateKind::Present => format!("option.is_some({subject})"),
                PredicateKind::Absent => format!("option.is_none({subject})"),
            })
        }
        Expr::CollectionPredicate {
            span,
            collection,
            quantifier,
            predicate,
        } => super::collection_predicates::render_collection_predicate(
            emitter,
            *span,
            collection,
            *quantifier,
            predicate,
            scope,
            prelude,
        ),
    }
}

fn render_binary(
    emitter: &mut Emitter<'_>,
    left: &Expr,
    op: BinaryOp,
    right: &Expr,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    if let Some((name, none_result)) = optional_short_circuit(left, op) {
        let option_ty = scope
            .get(name)
            .ok_or_else(|| EmitError::new(left.span(), format!("`{name}` is not in scope")))?;
        let GType::Option(inner) = emitter.env.resolve(option_ty) else {
            return Err(EmitError::new(
                left.span(),
                format!("`{name}` is not optional"),
            ));
        };
        let mut rhs_scope = scope.clone();
        rhs_scope.insert(name.to_owned(), *inner);
        let mut rhs_prelude = Vec::new();
        let rhs = render_expr(emitter, right, &rhs_scope, &mut rhs_prelude)?;
        let name = ident(name);
        let mut some_branch = String::new();
        if rhs_prelude.is_empty() {
            some_branch.push_str(&rhs);
        } else {
            some_branch.push_str("{\n");
            for line in rhs_prelude {
                let _ = writeln!(some_branch, "    {line}");
            }
            let _ = write!(some_branch, "    {rhs}\n  }}");
        }
        return Ok(format!(
            "case {name} {{\n  Some({name}) -> {some_branch}\n  None -> {}\n}}",
            if none_result { "True" } else { "False" }
        ));
    }
    let symbol = operator_for(emitter, op, left, right, scope);
    let left = render_parenthesized(emitter, left, scope, prelude)?;
    let right = render_parenthesized(emitter, right, scope, prelude)?;
    Ok(format!("{left} {symbol} {right}"))
}

/// Recognize the optional predicates whose short-circuit edge proves the RHS
/// binding is present. The returned boolean is the result for `None`.
fn optional_short_circuit(left: &Expr, op: BinaryOp) -> Option<(&str, bool)> {
    let expected = match op {
        BinaryOp::And => PredicateKind::Present,
        BinaryOp::Or => PredicateKind::Absent,
        _ => return None,
    };
    let Expr::Predicate { subject, kind, .. } = left else {
        return None;
    };
    let Expr::Ref { name, .. } = subject.as_ref() else {
        return None;
    };
    (*kind == expected).then_some((name, matches!(op, BinaryOp::Or)))
}

fn render_parenthesized(
    emitter: &mut Emitter<'_>,
    expr: &Expr,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    let rendered = render_expr(emitter, expr, scope, prelude)?;
    match expr {
        // Gleam groups expressions with blocks. Parentheses are reserved for
        // calls and tuple syntax, so `(a && b)` is not valid Gleam.
        Expr::Not { .. } | Expr::Binary { .. } | Expr::Predicate { .. } => {
            Ok(format!("{{ {rendered} }}"))
        }
        _ => Ok(rendered),
    }
}

/// The Gleam operator for a binary op. Gleam's bare ordering operators are
/// Int-only, so Float comparisons render the `.`-suffixed Float family (the
/// checker admits ordering on Int/Int and Float/Float alone, so one Float
/// operand settles the pair).
fn operator_for(
    emitter: &Emitter<'_>,
    op: BinaryOp,
    left: &Expr,
    right: &Expr,
    scope: &Scope,
) -> &'static str {
    let ordering = matches!(
        op,
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    );
    let float_operands = ordering
        && [left, right].into_iter().any(|side| {
            expr_type(emitter, side, scope)
                .is_ok_and(|ty| matches!(emitter.env.resolve(&ty), GType::Float))
        });
    match (op, float_operands) {
        (BinaryOp::Lt, true) => "<.",
        (BinaryOp::Le, true) => "<=.",
        (BinaryOp::Gt, true) => ">.",
        (BinaryOp::Ge, true) => ">=.",
        (BinaryOp::Or, _) => "||",
        (BinaryOp::And, _) => "&&",
        (BinaryOp::Eq, _) => "==",
        (BinaryOp::Ne, _) => "!=",
        (BinaryOp::Lt, false) => "<",
        (BinaryOp::Le, false) => "<=",
        (BinaryOp::Gt, false) => ">",
        (BinaryOp::Ge, false) => ">=",
        (BinaryOp::Concat, _) => "<>",
    }
}

/// Render a value destined for a typed slot, wrapping a present value in
/// `Some(…)` when the slot is optional and the value is not.
pub(super) fn render_arg_for(
    emitter: &mut Emitter<'_>,
    expr: &Expr,
    expected: &GType,
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    let rendered = render_expr(emitter, expr, scope, prelude)?;
    if matches!(emitter.env.resolve(expected), GType::Option(_)) {
        let actual = expr_type(emitter, expr, scope).unwrap_or(GType::Unknown);
        if !matches!(emitter.env.resolve(&actual), GType::Option(_)) {
            return Ok(format!("Some({rendered})"));
        }
    }
    Ok(rendered)
}

/// Render record construction with required named fields: declared fields in
/// declaration order, absent optional fields filled with `None`.
pub(super) fn render_record(
    emitter: &mut Emitter<'_>,
    span: Span,
    name: &str,
    name_span: Span,
    args: &[Arg],
    scope: &Scope,
    prelude: &mut Vec<String>,
) -> Result<String, EmitError> {
    let ty = GType::Named(name.to_owned());
    let Some((gleam_name, record)) = emitter.env.record_of(&ty) else {
        return Err(EmitError::new(
            name_span,
            format!("`{name}` is not a constructible record type"),
        ));
    };
    let fields = record.fields.clone();
    for arg in args {
        if !fields.iter().any(|field| field.awl_name == arg.name) {
            return Err(EmitError::new(
                arg.name_span,
                format!("`{name}` has no field `{}`", arg.name),
            ));
        }
    }
    if fields.is_empty() {
        return Ok(gleam_name);
    }
    let mut rendered = format!("{gleam_name}(");
    for (position, field) in fields.iter().enumerate() {
        if position > 0 {
            rendered.push_str(", ");
        }
        let value = match args.iter().find(|arg| arg.name == field.awl_name) {
            Some(arg) => render_arg_for(emitter, &arg.value, &field.ty, scope, prelude)?,
            None if matches!(emitter.env.resolve(&field.ty), GType::Option(_)) => "None".to_owned(),
            None => {
                return Err(EmitError::new(
                    span,
                    format!(
                        "constructing `{name}` misses its required field `{}`",
                        field.awl_name
                    ),
                ));
            }
        };
        let _ = write!(rendered, "{}: {value}", ident(&field.awl_name));
    }
    rendered.push(')');
    Ok(rendered)
}
