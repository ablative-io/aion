use crate::{BinaryOp, DurationLiteral, DurationUnit, Expr, RetrySpec, TypeRef};

pub(super) fn collect_named_ref(ty: &TypeRef, names: &mut Vec<String>) {
    match ty {
        TypeRef::Named { name, .. } => {
            if !names.iter().any(|seen| seen == name) {
                names.push(name.clone());
            }
        }
        TypeRef::List { inner, .. } | TypeRef::Option { inner, .. } => {
            collect_named_ref(inner, names);
        }
    }
}

/// Collect the reference names an expression mentions, in first-use order.
pub(super) fn collect_expr_refs(value: &Expr, names: &mut Vec<String>) {
    match value {
        Expr::String { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_) => {}
        Expr::List { items, .. } => {
            for item in items {
                collect_expr_refs(item, names);
            }
        }
        Expr::Ref { name, .. } => {
            if !names.iter().any(|seen| seen == name) {
                names.push(name.clone());
            }
        }
        Expr::Field { base, .. } => collect_expr_refs(base, names),
        Expr::Record { fields, .. } => {
            for field in fields {
                collect_expr_refs(&field.value, names);
            }
        }
        Expr::Not { expr: inner, .. } => collect_expr_refs(inner, names),
        Expr::Binary { left, right, .. } => {
            collect_expr_refs(left, names);
            collect_expr_refs(right, names);
        }
    }
}

pub(super) fn is_builtin_type(name: &str) -> bool {
    matches!(name, "String" | "Int" | "Float" | "Bool" | "Nil")
}

pub(super) fn collect_composite_type(ty: &TypeRef, types: &mut Vec<TypeRef>) {
    match ty {
        TypeRef::Named { .. } => {}
        TypeRef::List { inner, .. } | TypeRef::Option { inner, .. } => {
            collect_composite_type(inner, types);
            if !types.iter().any(|seen| codec_name(seen) == codec_name(ty)) {
                types.push(ty.clone());
            }
        }
    }
}

pub(super) fn gleam_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => name.clone(),
        TypeRef::List { inner, .. } => {
            let inner = gleam_type(inner);
            format!("List({inner})")
        }
        TypeRef::Option { inner, .. } => {
            let inner = gleam_type(inner);
            format!("Option({inner})")
        }
    }
}

pub(super) fn codec_name(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => snake(name),
        TypeRef::List { inner, .. } => {
            let inner = codec_name(inner);
            format!("list_{inner}")
        }
        TypeRef::Option { inner, .. } => {
            let inner = codec_name(inner);
            format!("option_{inner}")
        }
    }
}

pub(super) fn expr(value: &Expr) -> String {
    match value {
        Expr::String { value, .. } => string_lit(value),
        Expr::Int { value, .. } => value.to_string(),
        Expr::Float { value, .. } => value.clone(),
        Expr::Bool { value, .. } => if *value { "True" } else { "False" }.to_owned(),
        Expr::Duration(duration) => duration_expr(duration),
        Expr::List { items, .. } => {
            let values = items.iter().map(expr).collect::<Vec<_>>().join(", ");
            format!("[{values}]")
        }
        Expr::Ref { name, .. } => ident(name),
        Expr::Field { base, field, .. } => {
            let base = expr(base);
            let field = ident(field);
            format!("{base}.{field}")
        }
        Expr::Record { name, fields, .. } => {
            let ctor = constructor(name);
            if fields.is_empty() {
                return ctor;
            }
            let fields = fields
                .iter()
                .map(|field| {
                    let name = ident(&field.name);
                    let value = expr(&field.value);
                    format!("{name}: {value}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{ctor}({fields})")
        }
        Expr::Not { expr: inner, .. } => {
            let inner = parenthesized(inner);
            format!("!{inner}")
        }
        Expr::Binary {
            left, op, right, ..
        } => {
            let left = parenthesized(left);
            let op = binary_op(*op);
            let right = parenthesized(right);
            format!("{left} {op} {right}")
        }
    }
}

pub(super) fn parenthesized(value: &Expr) -> String {
    match value {
        Expr::String { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::List { .. }
        | Expr::Ref { .. }
        | Expr::Field { .. }
        | Expr::Record { .. } => expr(value),
        Expr::Not { .. } | Expr::Binary { .. } => {
            let value = expr(value);
            format!("({value})")
        }
    }
}

pub(super) fn binary_op(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "||",
        BinaryOp::And => "&&",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Add => "<>",
    }
}

pub(super) fn duration_ms(duration: &DurationLiteral) -> u64 {
    match duration.unit {
        DurationUnit::Seconds => duration.magnitude.saturating_mul(1_000),
        DurationUnit::Minutes => duration.magnitude.saturating_mul(60_000),
        DurationUnit::Hours => duration.magnitude.saturating_mul(3_600_000),
        DurationUnit::Days => duration.magnitude.saturating_mul(86_400_000),
    }
}

pub(super) fn duration_expr(duration: &DurationLiteral) -> String {
    let milliseconds = duration_ms(duration);
    format!("duration.milliseconds({milliseconds})")
}

pub(super) fn retry_policy(retry: &RetrySpec) -> String {
    match retry {
        RetrySpec::Every { count, every, .. } => format!(
            "activity.RetryPolicy(max_attempts: {}, backoff: activity.Fixed({}))",
            count,
            duration_expr(every)
        ),
        RetrySpec::Backoff {
            count, min, max, ..
        } => format!(
            "activity.RetryPolicy(max_attempts: {}, backoff: activity.Exponential(initial: {}, multiplier: 2.0, max: {}))",
            count,
            duration_expr(min),
            duration_expr(max)
        ),
    }
}

pub(super) fn string_lit(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

pub(super) fn constructor(name: &str) -> String {
    pascal(name)
}

pub(super) fn pascal(name: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for character in name.chars() {
        if character == '_' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    out
}

pub(super) fn snake(name: &str) -> String {
    let mut out = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// Reserved words in Gleam that cannot be used as value identifiers.
pub(super) fn is_gleam_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "assert"
            | "auto"
            | "case"
            | "const"
            | "delegate"
            | "derive"
            | "echo"
            | "else"
            | "fn"
            | "if"
            | "implement"
            | "import"
            | "let"
            | "macro"
            | "opaque"
            | "panic"
            | "pub"
            | "test"
            | "todo"
            | "type"
            | "use"
    )
}

/// Sanitize an AWL identifier for emission: Gleam reserved words gain a
/// trailing underscore, applied consistently at every emission site.
pub(super) fn ident(name: &str) -> String {
    if is_gleam_keyword(name) {
        format!("{name}_")
    } else {
        name.to_owned()
    }
}

pub(super) fn wrap_doc(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    vec![text.to_owned()]
}
