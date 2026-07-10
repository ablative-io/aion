//! Composite (list/option) and builtin leaf codecs. Options in non-field
//! positions keep the SDK's nullable form; since the 2026-07-11 ruling the
//! checker refuses `[T?]` (element-position `?`), so a checked document
//! only reaches this path with optional lists (`[T]?`) and other whole-value
//! options — the nullable rendering survives as the defensive lowering for
//! unchecked documents.

use std::collections::BTreeMap;

use super::context::Emitter;
use super::types::{GType, NamedDef, type_ref_to_g};

/// Every list/option shape reachable from a wire position gets a codec trio.
pub(super) fn composite_codecs(emitter: &mut Emitter<'_>) {
    let mut composites: BTreeMap<String, GType> = BTreeMap::new();
    let mut roots: Vec<GType> = Vec::new();
    for input in &emitter.document.inputs {
        roots.push(type_ref_to_g(&input.ty));
    }
    for outcome in &emitter.document.outcomes {
        roots.push(type_ref_to_g(&outcome.ty));
    }
    for signal in &emitter.document.signals {
        roots.push(type_ref_to_g(&signal.ty));
    }
    for worker in &emitter.document.workers {
        for action in &worker.actions {
            for param in &action.params {
                roots.push(type_ref_to_g(&param.ty));
            }
            roots.push(type_ref_to_g(&action.returns));
        }
    }
    for child in &emitter.document.children {
        for param in &child.params {
            roots.push(type_ref_to_g(&param.ty));
        }
        roots.push(type_ref_to_g(&child.returns));
    }
    for name in &emitter.env.order {
        if let Some(NamedDef::Record(record)) = emitter.env.get(name) {
            for field in &record.fields {
                roots.push(field.ty.clone());
            }
        }
    }
    for root in roots {
        collect_composites(emitter, &root, &mut composites);
    }
    for ty in composites.values() {
        match ty {
            GType::List(inner) => {
                let stem = emitter.env.codec_name(ty);
                let inner_stem = emitter.env.codec_name(inner);
                let rendered = emitter.env.gleam_type(ty);
                emitter.line(&format!("fn {stem}_codec() -> Codec({rendered}) {{"));
                emitter.indented(|this| {
                    this.line(&format!(
                        "codec.json_codec({stem}_to_json, {stem}_decoder())"
                    ));
                });
                emitter.line("}");
                emitter.line(&format!(
                    "fn {stem}_to_json(values: {rendered}) -> json.Json {{ \
                     json.array(values, {inner_stem}_to_json) }}"
                ));
                emitter.line(&format!(
                    "fn {stem}_decoder() -> decode.Decoder({rendered}) {{ \
                     decode.list({inner_stem}_decoder()) }}"
                ));
                emitter.blank();
            }
            GType::Option(inner) => {
                let stem = emitter.env.codec_name(ty);
                let inner_stem = emitter.env.codec_name(inner);
                let rendered = emitter.env.gleam_type(ty);
                emitter.line(&format!("fn {stem}_codec() -> Codec({rendered}) {{"));
                emitter.indented(|this| {
                    this.line(&format!(
                        "codec.json_codec({stem}_to_json, {stem}_decoder())"
                    ));
                });
                emitter.line("}");
                emitter.line(&format!(
                    "fn {stem}_to_json(value: {rendered}) -> json.Json {{ \
                     json.nullable(value, {inner_stem}_to_json) }}"
                ));
                emitter.line(&format!(
                    "fn {stem}_decoder() -> decode.Decoder({rendered}) {{ \
                     decode.optional({inner_stem}_decoder()) }}"
                ));
                emitter.blank();
            }
            _ => {}
        }
    }
}

fn collect_composites(emitter: &Emitter<'_>, ty: &GType, composites: &mut BTreeMap<String, GType>) {
    match ty {
        GType::List(inner) | GType::Option(inner) => {
            collect_composites(emitter, inner, composites);
            composites
                .entry(emitter.env.codec_name(ty))
                .or_insert_with(|| ty.clone());
        }
        GType::Named(name) => {
            if let Some(NamedDef::Alias(inner)) = emitter.env.get(name) {
                let inner = inner.clone();
                collect_composites(emitter, &inner, composites);
            }
        }
        _ => {}
    }
}

pub(super) fn builtin_codecs(emitter: &mut Emitter<'_>) {
    emitter.line(
        "fn string_codec() -> Codec(String) { codec.json_codec(json.string, decode.string) }",
    );
    emitter.line("fn int_codec() -> Codec(Int) { codec.json_codec(json.int, decode.int) }");
    emitter.line("fn float_codec() -> Codec(Float) { codec.json_codec(json.float, decode.float) }");
    emitter.line("fn bool_codec() -> Codec(Bool) { codec.json_codec(json.bool, decode.bool) }");
    emitter.line(
        "fn nil_codec() -> Codec(Nil) { codec.json_codec(fn(_) { json.object([]) }, \
         decode.success(Nil)) }",
    );
    emitter.blank();
    emitter.line("fn string_to_json(value: String) -> json.Json { json.string(value) }");
    emitter.line("fn int_to_json(value: Int) -> json.Json { json.int(value) }");
    emitter.line("fn float_to_json(value: Float) -> json.Json { json.float(value) }");
    emitter.line("fn bool_to_json(value: Bool) -> json.Json { json.bool(value) }");
    emitter.line("fn nil_to_json(_: Nil) -> json.Json { json.object([]) }");
    emitter.blank();
    emitter.line("fn string_decoder() -> decode.Decoder(String) { decode.string }");
    emitter.line("fn int_decoder() -> decode.Decoder(Int) { decode.int }");
    emitter.line("fn float_decoder() -> decode.Decoder(Float) { decode.float }");
    emitter.line("fn bool_decoder() -> decode.Decoder(Bool) { decode.bool }");
    emitter.line("fn nil_decoder() -> decode.Decoder(Nil) { decode.success(Nil) }");
    emitter.blank();
}
