//! Leaf renderers shared across the golden printer: values, live sets, type
//! descriptors, wire descriptors, and function-name resolution.

use super::super::ids::{FnRef, Var};
use super::super::ops::{LiveAfter, Value};
use super::super::runtime::DurableFamily;
use super::super::shapes::{MirLiteral, WireDesc};
use super::super::tydesc::TyDesc;
use super::super::unit::MirModule;

pub(super) const fn family_name(family: DurableFamily) -> &'static str {
    match family {
        DurableFamily::Timers => "timers",
        DurableFamily::Activities => "activities",
        DurableFamily::Children => "children",
        DurableFamily::Signals => "signals",
    }
}

pub(super) fn render_wire(desc: &WireDesc) -> String {
    match desc {
        WireDesc::Bool => "bool".to_owned(),
        WireDesc::Int => "int".to_owned(),
        WireDesc::Float => "float".to_owned(),
        WireDesc::Str => "string".to_owned(),
        WireDesc::Nil => "nil".to_owned(),
        WireDesc::List(inner) => format!("list({})", render_wire(inner)),
        WireDesc::Nullable(inner) => format!("nullable({})", render_wire(inner)),
        WireDesc::Ref(name) => format!("ref({name})"),
    }
}

/// Render one literal-pool entry's CONTENT (the R5 codec-identity pin
/// surface): `lit#N` operands stop being opaque because the module section
/// prints every pool entry through this.
pub(super) fn render_literal(module: &MirModule, literal: &MirLiteral) -> String {
    match literal {
        MirLiteral::Integer(value) => value.to_string(),
        MirLiteral::Float { lexeme } => format!("float({lexeme})"),
        MirLiteral::Atom(atom) => format!("'{}'", module.atom(atom.0).unwrap_or("?")),
        MirLiteral::Binary(bytes) => {
            if let Ok(text) = std::str::from_utf8(bytes) {
                format!("{text:?}")
            } else {
                use std::fmt::Write as _;
                let mut hex = String::from("0x");
                for byte in bytes {
                    let _ = write!(hex, "{byte:02x}");
                }
                hex
            }
        }
        MirLiteral::Nil => "nil".to_owned(),
        MirLiteral::Tuple(elements) => format!(
            "#({})",
            elements
                .iter()
                .map(|element| render_literal(module, element))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        MirLiteral::List(elements) => format!(
            "[{}]",
            elements
                .iter()
                .map(|element| render_literal(module, element))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub(super) fn render_values(module: &MirModule, values: &[Value]) -> String {
    values
        .iter()
        .map(|value| render_value(module, value))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn render_value(module: &MirModule, value: &Value) -> String {
    match value {
        Value::Var(v) => var(*v),
        Value::Lit(reference) => format!("lit#{}", reference.0),
        Value::Atom(atom) => format!("'{}'", module.atom(atom.0).unwrap_or("?")),
        Value::Int(value) => value.to_string(),
        Value::Nil => "nil".to_owned(),
    }
}

pub(super) fn render_live(live: &LiveAfter) -> String {
    if live.0.is_empty() {
        String::new()
    } else {
        format!(
            " live_after=[{}]",
            live.0
                .iter()
                .map(|v| var(*v))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

pub(super) fn var(v: Var) -> String {
    format!("v{}", v.0)
}

pub(super) fn opt_var(v: Option<Var>) -> String {
    v.map_or_else(|| "_".to_owned(), var)
}

pub(super) fn fn_name(module: &MirModule, reference: FnRef) -> String {
    module.function(reference).map_or_else(
        || format!("fn#{}", reference.0),
        |function| function.name().to_owned(),
    )
}

pub(super) fn render_tydesc(ty: &TyDesc) -> String {
    match ty {
        TyDesc::Bool => "Bool".to_owned(),
        TyDesc::Int => "Int".to_owned(),
        TyDesc::Float => "Float".to_owned(),
        TyDesc::String => "String".to_owned(),
        TyDesc::Nil => "Nil".to_owned(),
        TyDesc::List(inner) => format!("List({})", render_tydesc(inner)),
        TyDesc::Option(inner) => format!("Option({})", render_tydesc(inner)),
        TyDesc::Result(ok, err) => format!("Result({}, {})", render_tydesc(ok), render_tydesc(err)),
        TyDesc::Tuple(elements) => format!(
            "#({})",
            elements
                .iter()
                .map(render_tydesc)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TyDesc::Custom {
            module,
            name,
            params,
        } => {
            if params.is_empty() {
                format!("{module}.{name}")
            } else {
                format!(
                    "{module}.{name}({})",
                    params
                        .iter()
                        .map(render_tydesc)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        TyDesc::Fn(args, ret) => format!(
            "fn({}) -> {}",
            args.iter()
                .map(render_tydesc)
                .collect::<Vec<_>>()
                .join(", "),
            render_tydesc(ret)
        ),
        TyDesc::Dynamic => "Dynamic".to_owned(),
        TyDesc::Json => "Json".to_owned(),
        TyDesc::AwlError => "AwlError".to_owned(),
        TyDesc::Decoder(inner) => format!("Decoder({})", render_tydesc(inner)),
        TyDesc::Codec(inner) => format!("Codec({})", render_tydesc(inner)),
        TyDesc::Activity(input, output) => {
            format!(
                "Activity({}, {})",
                render_tydesc(input),
                render_tydesc(output)
            )
        }
        TyDesc::ChildHandle(output, error) => {
            format!(
                "ChildHandle({}, {})",
                render_tydesc(output),
                render_tydesc(error)
            )
        }
        TyDesc::SignalRef(inner) => format!("Signal({})", render_tydesc(inner)),
        TyDesc::WorkflowDefinition(input, output, error) => format!(
            "WorkflowDefinition({}, {}, {})",
            render_tydesc(input),
            render_tydesc(output),
            render_tydesc(error)
        ),
        TyDesc::Duration => "Duration".to_owned(),
        TyDesc::Unknown => "Unknown".to_owned(),
    }
}
