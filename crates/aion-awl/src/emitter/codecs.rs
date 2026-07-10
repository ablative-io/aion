//! Generated codecs: the error codec, the workflow input record, the
//! outcome union, every declared/projected record and enum, action input
//! records, composite (list/option) codecs, and the builtin leaf codecs.
//!
//! Optional record fields honor D4 (absence, never null): encoding omits an
//! absent field entirely; decoding uses `decode.optional_field`, so an
//! explicit `null` fails to decode. Options in non-field positions (list
//! elements) keep the SDK's nullable form — mirroring the checker's
//! recorded `[T?]` note awaiting a spec ruling.

use crate::RouteDirection;

use super::context::Emitter;
use super::error::EmitError;
use super::names::{ident, snake, string_lit};
use super::types::{GType, NamedDef, type_ref_to_g};

pub(super) fn emit_codecs(emitter: &mut Emitter<'_>) -> Result<(), EmitError> {
    awl_error_codec(emitter);

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
    super::composites::builtin_codecs(emitter);
    Ok(())
}

fn awl_error_codec(emitter: &mut Emitter<'_>) {
    const MESSAGE_VARIANTS: [&str; 7] = [
        "AwlDecodeInputFailed",
        "AwlActivityFailed",
        "AwlSignalFailed",
        "AwlChildFailed",
        "AwlTimerFailed",
        "AwlTimedOut",
        "AwlIndexOutOfRange",
    ];
    emitter.line("fn awl_error_codec() -> Codec(AwlError) {");
    emitter.indented(|this| {
        this.line("codec.json_codec(awl_error_to_json, awl_error_decoder())");
    });
    emitter.line("}");
    emitter.blank();
    emitter.line("fn awl_error_to_json(error_value: AwlError) -> json.Json {");
    emitter.indented(|this| {
        this.line("case error_value {");
        this.indented(|this| {
            for variant in MESSAGE_VARIANTS {
                this.line(&format!(
                    "{variant}(message) -> json.object([#(\"tag\", json.string(\"{variant}\")), \
                     #(\"message\", json.string(message))])"
                ));
            }
            this.line(
                "AwlOutcomeFailure(outcome, payload) -> json.object([#(\"tag\", \
                 json.string(\"AwlOutcomeFailure\")), #(\"outcome\", json.string(outcome)), \
                 #(\"payload\", json.string(payload))])",
            );
            this.line("AwlFailed -> json.object([#(\"tag\", json.string(\"AwlFailed\"))])");
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();
    emitter.line("fn awl_error_decoder() -> decode.Decoder(AwlError) {");
    emitter.indented(|this| {
        this.line("use tag <- decode.field(\"tag\", decode.string)");
        this.line("case tag {");
        this.indented(|this| {
            for variant in MESSAGE_VARIANTS {
                this.line(&format!("\"{variant}\" -> {{"));
                this.indented(|this| {
                    this.line("use message <- decode.field(\"message\", decode.string)");
                    this.line(&format!("decode.success({variant}(message))"));
                });
                this.line("}");
            }
            this.line("\"AwlOutcomeFailure\" -> {");
            this.indented(|this| {
                this.line("use outcome <- decode.field(\"outcome\", decode.string)");
                this.line("use payload <- decode.field(\"payload\", decode.string)");
                this.line("decode.success(AwlOutcomeFailure(outcome: outcome, payload: payload))");
            });
            this.line("}");
            this.line("\"AwlFailed\" -> decode.success(AwlFailed)");
            this.line("_ -> decode.failure(AwlFailed, \"AwlError\")");
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();
}

fn union_codec(emitter: &mut Emitter<'_>) -> Result<(), EmitError> {
    let Some(union_type) = emitter.union_type.clone() else {
        return Ok(());
    };
    let stem = snake(&union_type);
    let successes: Vec<(String, String, GType, String)> = emitter
        .document
        .outcomes
        .iter()
        .filter(|outcome| matches!(outcome.direction, RouteDirection::Success))
        .filter_map(|outcome| {
            let info = emitter.outcomes.get(outcome.name.as_str())?;
            let constructor = info.constructor.clone()?;
            Some((
                outcome.name.clone(),
                constructor,
                info.ty.clone(),
                emitter.env.codec_name(&info.ty),
            ))
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
            for (name, constructor, _, codec) in &successes {
                this.line(&format!(
                    "{constructor}(payload) -> json.object([#(\"outcome\", \
                     json.string({})), #(\"payload\", {codec}_to_json(payload))])",
                    string_lit(name)
                ));
            }
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();

    let Some((_, first_constructor, first_ty, _)) = successes.first().cloned() else {
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
            for (name, constructor, _, codec) in &successes {
                this.line(&format!("{} -> {{", string_lit(name)));
                this.indented(|this| {
                    this.line(&format!(
                        "use payload <- decode.field(\"payload\", {codec}_decoder())"
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
fn record_codec(emitter: &mut Emitter<'_>, name: &str, fields: &[(String, GType)]) {
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
                        let codec = this.env.codec_name(&inner);
                        this.line(&format!("case value.{access} {{"));
                        this.indented(|this| {
                            this.line(&format!(
                                "Some(inner) -> [#({}, {codec}_to_json(inner))]",
                                string_lit(field_name)
                            ));
                            this.line("None -> []");
                        });
                        this.line("},");
                    } else {
                        let codec = this.env.codec_name(field_ty);
                        this.line(&format!(
                            "[#({}, {codec}_to_json(value.{access}))],",
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
                    let codec = this.env.codec_name(field_ty);
                    let access = ident(field_name);
                    this.line(&format!(
                        "#({}, {codec}_to_json(value.{access})),",
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
                let codec = this.env.codec_name(&inner);
                this.line(&format!(
                    "use {binding} <- decode.optional_field({}, None, \
                     decode.map({codec}_decoder(), Some))",
                    string_lit(field_name)
                ));
            } else {
                let codec = this.env.codec_name(field_ty);
                this.line(&format!(
                    "use {binding} <- decode.field({}, {codec}_decoder())",
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
