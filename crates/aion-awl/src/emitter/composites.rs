//! Composite (list/option) codecs. Their inner leaf references resolve to the
//! hoisted `awlc.<leaf>_…` SDK functions (AWL-BC-0); named inners stay
//! module-generated. Options in non-field
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
                let inner_to_json = emitter.to_json_fn(inner);
                let inner_decoder = emitter.decoder_fn(inner);
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
                     json.array(values, {inner_to_json}) }}"
                ));
                emitter.line(&format!(
                    "fn {stem}_decoder() -> decode.Decoder({rendered}) {{ \
                     decode.list({inner_decoder}()) }}"
                ));
                emitter.blank();
            }
            GType::Option(inner) => {
                let stem = emitter.env.codec_name(ty);
                let inner_to_json = emitter.to_json_fn(inner);
                let inner_decoder = emitter.decoder_fn(inner);
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
                     json.nullable(value, {inner_to_json}) }}"
                ));
                emitter.line(&format!(
                    "fn {stem}_decoder() -> decode.Decoder({rendered}) {{ \
                     decode.optional({inner_decoder}()) }}"
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
