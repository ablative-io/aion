//! Text rendering for expressions, type references, durations, and
//! arguments — the leaf vocabulary shared by every printed construct.

use crate::DurationUnit;
use crate::ast::{Arg, BinaryOp, DurationLiteral, Expr, PredicateKind, RetrySpec, TypeRef};

pub(super) fn expr_text(expr: &Expr) -> String {
    match expr {
        Expr::String { value, .. } => string_literal(value),
        Expr::Int { value, .. } => value.to_string(),
        Expr::Float { value, .. } => value.clone(),
        Expr::Bool { value, .. } => value.to_string(),
        Expr::Duration(duration) => duration_text(duration),
        Expr::List { items, .. } => {
            let items: Vec<String> = items.iter().map(expr_text).collect();
            format!("[{}]", items.join(", "))
        }
        Expr::Ref { name, .. } | Expr::Variant { name, .. } => name.clone(),
        Expr::Record { name, args, .. } => format!("{name}({})", args_text(args)),
        Expr::Field { base, name, .. } => format!("{}.{name}", expr_text(base)),
        Expr::Index { base, index, .. } => format!("{}[{index}]", expr_text(base)),
        Expr::Accessor { name, .. } => format!(".{name}"),
        Expr::Not { expr, .. } => format!("not {}", expr_text(expr)),
        Expr::Binary {
            left, op, right, ..
        } => format!(
            "{} {} {}",
            expr_text(left),
            binary_op_text(*op),
            expr_text(right)
        ),
        Expr::Predicate { subject, kind, .. } => format!(
            "{} is {}",
            expr_text(subject),
            match kind {
                PredicateKind::Empty => "empty",
                PredicateKind::Present => "present",
                PredicateKind::Absent => "absent",
            }
        ),
    }
}

pub(super) fn args_text(args: &[Arg]) -> String {
    let rendered: Vec<String> = args.iter().map(arg_text).collect();
    rendered.join(", ")
}

pub(super) fn arg_text(arg: &Arg) -> String {
    format!("{}: {}", arg.name, expr_text(&arg.value))
}

pub(super) fn string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

const fn binary_op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "or",
        BinaryOp::And => "and",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Concat => "+",
    }
}

pub(super) fn duration_text(duration: &DurationLiteral) -> String {
    let unit = match duration.unit {
        DurationUnit::Seconds => "s",
        DurationUnit::Minutes => "m",
        DurationUnit::Hours => "h",
        DurationUnit::Days => "d",
    };
    format!("{}{unit}", duration.magnitude)
}

pub(super) fn type_ref_text(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => name.clone(),
        TypeRef::List { inner, .. } => format!("[{}]", type_ref_text(inner)),
        TypeRef::Optional { inner, .. } => format!("{}?", type_ref_text(inner)),
    }
}

pub(super) fn retry_text(retry: &RetrySpec) -> String {
    match retry {
        RetrySpec::Every { count, every, .. } => {
            format!("retry {count} every {}", duration_text(every))
        }
        RetrySpec::Backoff {
            count, min, max, ..
        } => format!(
            "retry {count} backoff {}..{}",
            duration_text(min),
            duration_text(max)
        ),
    }
}

/// Column width of `text` in characters (the 100-column rule counts
/// characters, not bytes, matching editor columns).
pub(super) fn width(text: &str) -> usize {
    text.chars().count()
}
