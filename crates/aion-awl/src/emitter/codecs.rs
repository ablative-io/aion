//! Generated codecs: the workflow input record, the outcome union, every
//! declared/projected record and enum, action input records, and composite
//! (list/option) codecs. Child output codecs strictly decode the child's AWL
//! `{outcome, payload}` result envelope to the payload type declared by the
//! parent. Their encode side uses the fixed neutral outcome name `child`, so
//! both backends expose one byte-identical symmetric wire contract.
//! The workflow-error codec and the builtin leaf codecs
//! are hoisted into the `aion/awl` SDK (AWL-BC-0); leaf references here resolve
//! to `awlc.<leaf>_…` via [`Emitter::to_json_fn`]/`decoder_fn`.
//!
//! Optional record fields honor D4 (absence, never null): encoding omits an
//! absent field entirely; decoding uses `decode.optional_field`, so an
//! explicit `null` fails to decode. Element-position `?` (`[T?]`) is refused
//! by the checker (ruled 2026-07-11); the remaining non-field options
//! (optional lists and other whole values) keep the SDK's nullable form.

use std::collections::BTreeMap;

use crate::RouteDirection;

use super::context::Emitter;
use super::error::EmitError;
use super::names::{ident, snake, string_lit};
use super::types::{GType, NamedDef, type_ref_to_g};

pub(super) fn emit_codecs(emitter: &mut Emitter<'_>) -> Result<(), EmitError> {
    // Workflow input record.
    let input_fields: Vec<(String, GType)> = emitter
        .document
        .inputs
        .iter()
        .map(|input| (input.name.clone(), type_ref_to_g(&input.ty)))
        .collect();
    let input_type = emitter.input_type.clone();
    record_codec(emitter, &input_type, &input_fields);

    // Outcome union.
    union_codec(emitter)?;

    // Declared and projected named types.
    for name in emitter.env.order.clone() {
        match emitter.env.get(&name).cloned() {
            Some(NamedDef::Record(record)) => {
                let fields: Vec<(String, GType)> = record
                    .fields
                    .iter()
                    .map(|field| (field.awl_name.clone(), field.ty.clone()))
                    .collect();
                record_codec(emitter, &name, &fields);
            }
            Some(NamedDef::Enum(variants)) => enum_codec(emitter, &name, &variants),
            Some(NamedDef::Alias(_)) | None => {}
        }
    }

    // Action input records.
    let document = emitter.document;
    for worker in &document.workers {
        for action in &worker.actions {
            let fields: Vec<(String, GType)> = action
                .params
                .iter()
                .map(|param| (param.name.clone(), type_ref_to_g(&param.ty)))
                .collect();
            let Some(input_name) = emitter.action_inputs.get(&action.name).cloned() else {
                continue;
            };
            record_codec(emitter, &input_name, &fields);
        }
    }

    super::composites::composite_codecs(emitter);
    child_output_codecs(emitter);
    Ok(())
}

/// Emit one strict AWL outcome-envelope codec per distinct declared child
/// payload type. The outcome string is required but deliberately unconstrained;
/// the parent's `-> T` contract selects only the required typed payload.
fn child_output_codecs(emitter: &mut Emitter<'_>) {
    let mut outputs: BTreeMap<String, GType> = emitter
        .document
        .children
        .iter()
        .map(|child| {
            let ty = type_ref_to_g(&child.returns);
            (emitter.env.codec_name(&ty), ty)
        })
        .collect();
    for (stem, ty) in emitter.implicit_child_outputs.clone() {
        outputs.entry(stem).or_insert(ty);
    }
    for (payload_stem, ty) in outputs {
        child_output_codec(emitter, &payload_stem, &ty);
    }
}

fn child_output_codec(emitter: &mut Emitter<'_>, payload_stem: &str, ty: &GType) {
    let stem = format!("awl_child_output_{payload_stem}");
    let gleam_type = emitter.env.gleam_type(ty);
    let to_json = emitter.to_json_fn(ty);
    let decoder = emitter.decoder_fn(ty);
    emitter.line(&format!("fn {stem}_codec() -> Codec({gleam_type}) {{"));
    emitter.indented(|this| {
        this.line(&format!(
            "codec.json_codec({stem}_to_json, {stem}_decoder())"
        ));
    });
    emitter.line("}");
    emitter.blank();
    emitter.line(&format!(
        "fn {stem}_to_json(payload: {gleam_type}) -> json.Json {{"
    ));
    emitter.indented(|this| {
        this.line(&format!(
            "json.object([#(\"outcome\", json.string(\"child\")), #(\"payload\", \
             {to_json}(payload))])"
        ));
    });
    emitter.line("}");
    emitter.blank();
    emitter.line(&format!(
        "fn {stem}_decoder() -> decode.Decoder({gleam_type}) {{"
    ));
    emitter.indented(|this| {
        this.line("use _outcome <- decode.field(\"outcome\", decode.string)");
        this.line(&format!(
            "use payload <- decode.field(\"payload\", {decoder}())"
        ));
        this.line("decode.success(payload)");
    });
    emitter.line("}");
    emitter.blank();
}

fn union_codec(emitter: &mut Emitter<'_>) -> Result<(), EmitError> {
    let Some(union_type) = emitter.union_type.clone() else {
        return Ok(());
    };
    let stem = snake(&union_type);
    let successes: Vec<(String, String, GType)> = emitter
        .document
        .outcomes
        .iter()
        .filter(|outcome| matches!(outcome.direction, RouteDirection::Success))
        .filter_map(|outcome| {
            let info = emitter.outcomes.get(outcome.name.as_str())?;
            let constructor = info.constructor.clone()?;
            Some((outcome.name.clone(), constructor, info.ty.clone()))
        })
        .collect();

    emitter.line(&format!("fn {stem}_codec() -> Codec({union_type}) {{"));
    emitter.indented(|this| {
        this.line(&format!(
            "codec.json_codec({stem}_to_json, {stem}_decoder())"
        ));
    });
    emitter.line("}");
    emitter.blank();
    emitter.line(&format!(
        "fn {stem}_to_json(value: {union_type}) -> json.Json {{"
    ));
    emitter.indented(|this| {
        this.line("case value {");
        this.indented(|this| {
            for (name, constructor, ty) in &successes {
                let to_json = this.to_json_fn(ty);
                this.line(&format!(
                    "{constructor}(payload) -> json.object([#(\"outcome\", \
                     json.string({})), #(\"payload\", {to_json}(payload))])",
                    string_lit(name)
                ));
            }
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();

    let Some((_, first_constructor, first_ty)) = successes.first().cloned() else {
        return Ok(());
    };
    let zero = emitter.env.zero_expr(&first_ty, emitter.document.span)?;
    emitter.line(&format!(
        "fn {stem}_decoder() -> decode.Decoder({union_type}) {{"
    ));
    emitter.indented(|this| {
        this.line("use outcome <- decode.field(\"outcome\", decode.string)");
        this.line("case outcome {");
        this.indented(|this| {
            for (name, constructor, ty) in &successes {
                let decoder = this.decoder_fn(ty);
                this.line(&format!("{} -> {{", string_lit(name)));
                this.indented(|this| {
                    this.line(&format!(
                        "use payload <- decode.field(\"payload\", {decoder}())"
                    ));
                    this.line(&format!("decode.success({constructor}(payload))"));
                });
                this.line("}");
            }
            this.line(&format!(
                "_ -> decode.failure({first_constructor}({zero}), \"{union_type}\")"
            ));
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();
    Ok(())
}

/// One record's codec trio, with optional fields omitted when absent.
pub(super) fn record_codec(emitter: &mut Emitter<'_>, name: &str, fields: &[(String, GType)]) {
    let stem = snake(name);
    let has_optional = fields
        .iter()
        .any(|(_, ty)| matches!(emitter.env.resolve(ty), GType::Option(_)));
    if has_optional {
        emitter.flags.uses_list_module = true;
    }
    emitter.line(&format!("fn {stem}_codec() -> Codec({name}) {{"));
    emitter.indented(|this| {
        this.line(&format!(
            "codec.json_codec({stem}_to_json, {stem}_decoder())"
        ));
    });
    emitter.line("}");
    emitter.blank();
    record_to_json(emitter, name, &stem, fields, has_optional);
    record_decoder(emitter, name, &stem, fields);
}

fn record_to_json(
    emitter: &mut Emitter<'_>,
    name: &str,
    stem: &str,
    fields: &[(String, GType)],
    has_optional: bool,
) {
    if fields.is_empty() {
        emitter.line(&format!("fn {stem}_to_json(_: {name}) -> json.Json {{"));
        emitter.indented(|this| this.line("json.object([])"));
        emitter.line("}");
    } else if has_optional {
        emitter.line(&format!("fn {stem}_to_json(value: {name}) -> json.Json {{"));
        emitter.indented(|this| {
            this.line("json.object(list.flatten([");
            this.indented(|this| {
                for (field_name, field_ty) in fields {
                    let access = ident(field_name);
                    if let GType::Option(inner) = this.env.resolve(field_ty) {
                        let to_json = this.to_json_fn(&inner);
                        this.line(&format!("case value.{access} {{"));
                        this.indented(|this| {
                            this.line(&format!(
                                "Some(inner) -> [#({}, {to_json}(inner))]",
                                string_lit(field_name)
                            ));
                            this.line("None -> []");
                        });
                        this.line("},");
                    } else {
                        let to_json = this.to_json_fn(field_ty);
                        this.line(&format!(
                            "[#({}, {to_json}(value.{access}))],",
                            string_lit(field_name)
                        ));
                    }
                }
            });
            this.line("]))");
        });
        emitter.line("}");
    } else {
        emitter.line(&format!("fn {stem}_to_json(value: {name}) -> json.Json {{"));
        emitter.indented(|this| {
            this.line("json.object([");
            this.indented(|this| {
                for (field_name, field_ty) in fields {
                    let to_json = this.to_json_fn(field_ty);
                    let access = ident(field_name);
                    this.line(&format!(
                        "#({}, {to_json}(value.{access})),",
                        string_lit(field_name)
                    ));
                }
            });
            this.line("])");
        });
        emitter.line("}");
    }
    emitter.blank();
}

fn record_decoder(emitter: &mut Emitter<'_>, name: &str, stem: &str, fields: &[(String, GType)]) {
    emitter.line(&format!("fn {stem}_decoder() -> decode.Decoder({name}) {{"));
    emitter.indented(|this| {
        for (field_name, field_ty) in fields {
            let binding = ident(field_name);
            if let GType::Option(inner) = this.env.resolve(field_ty) {
                let decoder = this.decoder_fn(&inner);
                this.line(&format!(
                    "use {binding} <- decode.optional_field({}, None, \
                     decode.map({decoder}(), Some))",
                    string_lit(field_name)
                ));
            } else {
                let decoder = this.decoder_fn(field_ty);
                this.line(&format!(
                    "use {binding} <- decode.field({}, {decoder}())",
                    string_lit(field_name)
                ));
            }
        }
        if fields.is_empty() {
            this.line(&format!("decode.success({name})"));
        } else {
            this.line(&format!("decode.success({name}("));
            this.indented(|this| {
                for (field_name, _) in fields {
                    let binding = ident(field_name);
                    this.line(&format!("{binding}: {binding},"));
                }
            });
            this.line("))");
        }
    });
    emitter.line("}");
    emitter.blank();
}

fn enum_codec(emitter: &mut Emitter<'_>, name: &str, variants: &[String]) {
    let stem = snake(name);
    emitter.line(&format!("fn {stem}_codec() -> Codec({name}) {{"));
    emitter.indented(|this| {
        this.line(&format!(
            "codec.json_codec({stem}_to_json, {stem}_decoder())"
        ));
    });
    emitter.line("}");
    emitter.blank();
    emitter.line(&format!("fn {stem}_to_json(value: {name}) -> json.Json {{"));
    emitter.indented(|this| {
        this.line("case value {");
        this.indented(|this| {
            for variant in variants {
                this.line(&format!("{variant} -> json.string(\"{variant}\")"));
            }
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();
    emitter.line(&format!("fn {stem}_decoder() -> decode.Decoder({name}) {{"));
    emitter.indented(|this| {
        this.line("use value <- decode.then(decode.string)");
        this.line("case value {");
        this.indented(|this| {
            for variant in variants {
                this.line(&format!("\"{variant}\" -> decode.success({variant})"));
            }
            if let Some(first) = variants.first() {
                this.line(&format!("_ -> decode.failure({first}, \"{name}\")"));
            }
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();
}
