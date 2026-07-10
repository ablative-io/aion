//! The generated module's frame: narration header, imports, the `AwlError`
//! type, all type declarations, `definition()`, and `run()`.

use crate::RouteDirection;

use super::context::Emitter;
use super::names::{ident, snake};
use super::types::{GType, NamedDef};

pub(super) fn header(emitter: &mut Emitter<'_>) {
    if emitter.document.narration.is_empty() {
        emitter.line("//// Generated from an AWL document.");
    } else {
        for line in &emitter.document.narration {
            let text = line.text.strip_prefix(' ').unwrap_or(&line.text);
            emitter.line(&format!("//// {text}"));
        }
    }
    emitter.blank();
    emitter.line("import aion/activity");
    emitter.line("import aion/codec.{type Codec}");
    emitter.line("import aion/duration");
    emitter.line("import aion/error");
    emitter.line("import aion/signal");
    emitter.line("import aion/workflow");
    if emitter.flags.compare_modules.contains("bool") {
        emitter.line("import gleam/bool");
    }
    emitter.line("import gleam/dynamic.{type Dynamic}");
    emitter.line("import gleam/dynamic/decode");
    if emitter.flags.compare_modules.contains("float") {
        emitter.line("import gleam/float");
    }
    if emitter.flags.compare_modules.contains("int") {
        emitter.line("import gleam/int");
    }
    emitter.line("import gleam/json");
    emitter.line("import gleam/list");
    emitter.line("import gleam/option.{type Option, None, Some}");
    if emitter.flags.compare_modules.contains("string") {
        emitter.line("import gleam/string");
    }
    emitter.blank();
}

pub(super) fn error_type(emitter: &mut Emitter<'_>) {
    emitter.line("pub type AwlError {");
    emitter.indented(|this| {
        this.line("AwlDecodeInputFailed(String)");
        this.line("AwlActivityFailed(String)");
        this.line("AwlSignalFailed(String)");
        this.line("AwlChildFailed(String)");
        this.line("AwlTimerFailed(String)");
        this.line("AwlTimedOut(String)");
        this.line("AwlIndexOutOfRange(String)");
        this.line("AwlOutcomeFailure(outcome: String, payload: String)");
        this.line("AwlFailed");
    });
    emitter.line("}");
    emitter.blank();
}

/// Emit one Gleam record type declaration.
pub(super) fn emit_record_type(emitter: &mut Emitter<'_>, name: &str, fields: &[(String, GType)]) {
    emitter.line(&format!("pub type {name} {{"));
    emitter.indented(|this| {
        if fields.is_empty() {
            this.line(name);
        } else {
            this.line(&format!("{name}("));
            this.indented(|this| {
                for (field_name, field_ty) in fields {
                    let rendered = this.env.gleam_type(field_ty);
                    this.line(&format!("{}: {rendered},", ident(field_name)));
                }
            });
            this.line(")");
        }
    });
    emitter.line("}");
    emitter.blank();
}

/// All type declarations: declared/projected records and enums, the input
/// record, and the outcome union.
pub(super) fn type_decls(emitter: &mut Emitter<'_>) {
    for name in emitter.env.order.clone() {
        let docs: Vec<String> = emitter
            .document
            .types
            .iter()
            .find(|decl| decl.name == name)
            .map(|decl| {
                decl.docs
                    .iter()
                    .map(|line| line.text.strip_prefix(' ').unwrap_or(&line.text).to_owned())
                    .collect()
            })
            .unwrap_or_default();
        match emitter.env.get(&name).cloned() {
            Some(NamedDef::Record(record)) => {
                for doc in &docs {
                    emitter.line(&format!("/// {doc}"));
                }
                let fields: Vec<(String, GType)> = record
                    .fields
                    .iter()
                    .map(|field| (field.awl_name.clone(), field.ty.clone()))
                    .collect();
                emit_record_type(emitter, &name, &fields);
            }
            Some(NamedDef::Enum(variants)) => {
                for doc in &docs {
                    emitter.line(&format!("/// {doc}"));
                }
                emitter.line(&format!("pub type {name} {{"));
                emitter.indented(|this| {
                    for variant in &variants {
                        this.line(variant);
                    }
                });
                emitter.line("}");
                emitter.blank();
            }
            Some(NamedDef::Alias(_)) | None => {}
        }
    }

    let input_fields: Vec<(String, GType)> = emitter
        .document
        .inputs
        .iter()
        .map(|input| (input.name.clone(), super::types::type_ref_to_g(&input.ty)))
        .collect();
    let input_type = emitter.input_type.clone();
    emit_record_type(emitter, &input_type, &input_fields);

    if let Some(union_type) = emitter.union_type.clone() {
        emitter.line(&format!("pub type {union_type} {{"));
        let outcomes: Vec<(String, GType)> = emitter
            .document
            .outcomes
            .iter()
            .filter(|outcome| matches!(outcome.direction, RouteDirection::Success))
            .filter_map(|outcome| {
                emitter
                    .outcomes
                    .get(outcome.name.as_str())
                    .and_then(|info| info.constructor.clone().map(|ctor| (ctor, info.ty.clone())))
            })
            .collect();
        emitter.indented(|this| {
            for (constructor, ty) in &outcomes {
                let payload = this.env.gleam_type(ty);
                this.line(&format!("{constructor}({payload})"));
            }
        });
        emitter.line("}");
        emitter.blank();
    }
}

pub(super) fn definition(emitter: &mut Emitter<'_>) {
    let input_type = emitter.input_type.clone();
    let output_type = emitter.output_type();
    let output_codec = match &emitter.union_type {
        Some(union_type) => format!("{}_codec", snake(union_type)),
        None => "nil_codec".to_owned(),
    };
    emitter.line("/// Typed definition binding the codecs to the execute function.");
    emitter.line(&format!(
        "pub fn definition() -> workflow.WorkflowDefinition({input_type}, {output_type}, \
         AwlError) {{"
    ));
    let workflow_name = emitter.document.name.clone();
    let input_codec = snake(&input_type);
    emitter.indented(|this| {
        this.line("workflow.define(");
        this.indented(|this| {
            this.line(&format!("\"{workflow_name}\","));
            this.line(&format!("{input_codec}_codec(),"));
            this.line(&format!("{output_codec}(),"));
            this.line("awl_error_codec(),");
            this.line("execute,");
        });
        this.line(")");
    });
    emitter.line("}");
    emitter.blank();
}

pub(super) fn run(emitter: &mut Emitter<'_>) {
    let input_codec = snake(&emitter.input_type);
    let output_codec = match &emitter.union_type {
        Some(union_type) => format!("{}_codec", snake(union_type)),
        None => "nil_codec".to_owned(),
    };
    emitter.line("/// Engine entry point.");
    emitter.line("pub fn run(raw_input: Dynamic) -> Result(String, AwlError) {");
    emitter.indented(|this| {
        this.line("case decode.run(raw_input, decode.string) {");
        this.indented(|this| {
            this.line("Ok(raw_json) ->");
            this.indented(|this| {
                this.line(&format!("case {input_codec}_codec().decode(raw_json) {{"));
                this.indented(|this| {
                    this.line("Ok(input) ->");
                    this.indented(|this| {
                        this.line("case execute(input) {");
                        this.indented(|this| {
                            this.line(&format!(
                                "Ok(result) -> Ok({output_codec}().encode(result))"
                            ));
                            this.line("Error(workflow_error) -> Error(workflow_error)");
                        });
                        this.line("}");
                    });
                    this.line("Error(codec.DecodeError(reason: reason, path: _)) ->");
                    this.indented(|this| {
                        this.line(
                            "Error(AwlDecodeInputFailed(\"failed to decode workflow input: \" \
                             <> reason))",
                        );
                    });
                });
                this.line("}");
            });
            this.line(
                "Error(_) -> Error(AwlDecodeInputFailed(\"workflow input payload was not a \
                 string\"))",
            );
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();
}
