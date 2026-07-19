//! Record-shaped projection of JSON Schema documents into checker types.
//!
//! Both schema doors (inline `schema { … }` and `schema("file")`) project
//! through the same rules: `object`/`properties`/`required`, nested objects,
//! arrays, string enums, and `$defs`-local `$ref`. Structural keywords the
//! model cannot honor are check errors naming the keyword and its JSON path;
//! constraint keywords are ignored for typing (and preserved on re-emit by
//! the schema module, which never consults this projection).

use std::fs;
use std::path::{Component, Path};
use std::rc::Rc;

use serde_json::{Map, Value};

use crate::Span;
use crate::ast::{TypeBody, TypeDecl};

use super::context::Ctx;
use super::types::{EnumTy, FieldTy, RecordTy, Ty};

/// Structural JSON Schema keywords the projection refuses, by design.
const UNSUPPORTED_KEYWORDS: [&str; 13] = [
    "oneOf",
    "anyOf",
    "allOf",
    "not",
    "if",
    "then",
    "else",
    "patternProperties",
    "prefixItems",
    "unevaluatedProperties",
    "dependentSchemas",
    "dependentRequired",
    "propertyNames",
];

/// Where a schema door's text came from, for spans and messages.
struct DoorSource<'a> {
    /// Import path or `schema { … }` description for messages.
    label: String,
    /// Span of the declaration's path literal (imports).
    decl_span: Span,
    /// Raw inline body and its span, for line-accurate inline anchors.
    inline: Option<(&'a str, Span)>,
}

impl DoorSource<'_> {
    /// Anchor a diagnostic: imports anchor at the declaration line; inline
    /// doors anchor at the `token` occurrence inside the value the failing
    /// JSON `path` names (never at an earlier occurrence of the same token
    /// elsewhere in the body).
    fn anchor(&self, token: &str, path: &[String]) -> Span {
        let Some((body, body_span)) = self.inline else {
            return self.decl_span;
        };
        let Some((offset, len)) = super::anchor::locate(body, path, token) else {
            return body_span;
        };
        let prefix = &body[..offset];
        let line_offset = prefix.matches('\n').count();
        let column = match prefix.rfind('\n') {
            Some(at) => prefix[at + 1..].chars().count() + 1,
            None => body_span.column + prefix.chars().count(),
        };
        Span {
            start: body_span.start + offset,
            end: body_span.start + offset + len,
            line: body_span.line + line_offset,
            column,
        }
    }
}

/// Project a schema-door type declaration into a semantic type, reporting
/// every projection error against the declaration.
pub(super) fn project_door(ctx: &mut Ctx<'_>, decl: &TypeDecl) -> Ty {
    let (value, source) = match &decl.body {
        TypeBody::SchemaInline { body, body_span } => {
            let source = DoorSource {
                label: format!("inline schema for `{}`", decl.name),
                decl_span: decl.name_span,
                inline: Some((body.as_str(), *body_span)),
            };
            match serde_json::from_str::<Value>(body) {
                Ok(value) => (value, source),
                Err(error) => {
                    ctx.error(
                        *body_span,
                        format!(
                            "the inline schema for `{}` is not valid JSON: {error}",
                            decl.name
                        ),
                    );
                    return Ty::Unknown;
                }
            }
        }
        TypeBody::SchemaImport { path, path_span } => {
            let source = DoorSource {
                label: format!("imported schema `{path}`"),
                decl_span: *path_span,
                inline: None,
            };
            match load_import(ctx, path, *path_span) {
                Some(value) => (value, source),
                None => return Ty::Unknown,
            }
        }
        TypeBody::Record { .. } | TypeBody::Enum { .. } => return Ty::Unknown,
    };

    let defs = value
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut projector = Projector {
        ctx,
        source,
        defs,
        failed: false,
    };
    let ty = projector.project(&value, &mut vec![], Some(decl.name.clone()), 0);
    if projector.failed { Ty::Unknown } else { ty }
}

fn load_import(ctx: &mut Ctx<'_>, path: &str, span: Span) -> Option<Value> {
    let import = Path::new(path);
    if path.is_empty()
        || import
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        ctx.error(
            span,
            format!(
                "imported schema `{path}` must be a non-empty relative path with no `..` components"
            ),
        );
        return None;
    }
    let Some(root) = ctx.root else {
        ctx.error(
            span,
            format!(
                "cannot resolve imported schema `{path}` without the document's directory \
                 (check the file from its own directory)"
            ),
        );
        return None;
    };
    let full = root.join(path);
    let text = match fs::read_to_string(&full) {
        Ok(text) => text,
        Err(error) => {
            ctx.error(
                span,
                format!("cannot read imported schema `{path}`: {error}"),
            );
            return None;
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            ctx.error(
                span,
                format!("imported schema `{path}` is not valid JSON: {error}"),
            );
            None
        }
    }
}

struct Projector<'c, 'a, 's> {
    ctx: &'c mut Ctx<'a>,
    source: DoorSource<'s>,
    defs: Map<String, Value>,
    failed: bool,
}

impl Projector<'_, '_, '_> {
    fn fail(&mut self, token: &str, path: &[String], detail: &str) -> Ty {
        let json_path = if path.is_empty() {
            "the schema root".to_owned()
        } else {
            format!("`{}`", path.join("."))
        };
        let span = self.source.anchor(token, path);
        let label = &self.source.label;
        self.ctx
            .error(span, format!("{label}: {detail} at {json_path}"));
        self.failed = true;
        Ty::Unknown
    }

    fn project(
        &mut self,
        schema: &Value,
        path: &mut Vec<String>,
        name: Option<String>,
        depth: usize,
    ) -> Ty {
        if depth > 24 {
            return self.fail(
                "$ref",
                path,
                "the schema nests or self-references too deeply",
            );
        }
        let Some(object) = schema.as_object() else {
            return self.fail("type", path, "expected a schema object");
        };
        for keyword in UNSUPPORTED_KEYWORDS {
            if object.contains_key(keyword) {
                return self.fail(
                    keyword,
                    path,
                    &format!(
                        "the record-shaped projection cannot honor the JSON Schema \
                         keyword `{keyword}`"
                    ),
                );
            }
        }
        if let Some(reference) = object.get("$ref") {
            return self.project_ref(reference, path, depth);
        }
        match object.get("type") {
            Some(Value::String(kind)) => match kind.as_str() {
                "object" => self.project_object(object, path, name, depth),
                "array" => self.project_array(object, path, depth),
                "string" => project_string(object, name),
                "integer" => Ty::Int,
                "number" => Ty::Float,
                "boolean" => Ty::Bool,
                "null" => self.fail(
                    "null",
                    path,
                    "`\"type\": \"null\"` cannot be projected — null does not exist in AWL; \
                     absence is an optional (`?`) field",
                ),
                other => self.fail("type", path, &format!("unsupported `type` `{other}`")),
            },
            Some(Value::Array(kinds)) => {
                if kinds.iter().any(|kind| kind == "null") {
                    self.fail(
                        "null",
                        path,
                        "a `null`-admitting union type cannot be projected — null does not \
                         exist in AWL; absence is an optional (`?`) field",
                    )
                } else {
                    self.fail("type", path, "union `type` arrays cannot be projected")
                }
            }
            _ if object.contains_key("properties") => {
                self.project_object(object, path, name, depth)
            }
            _ if object.contains_key("enum") => project_string(object, name),
            _ => Ty::Unknown,
        }
    }

    fn project_ref(&mut self, reference: &Value, path: &mut Vec<String>, depth: usize) -> Ty {
        let Some(target) = reference.as_str() else {
            return self.fail("$ref", path, "`$ref` must be a string");
        };
        let Some(def_name) = target.strip_prefix("#/$defs/") else {
            return self.fail(
                "$ref",
                path,
                &format!("only `$defs`-local `$ref` targets are supported, found `{target}`"),
            );
        };
        let Some(definition) = self.defs.get(def_name).cloned() else {
            return self.fail(
                "$ref",
                path,
                &format!("`$ref` names a missing `$defs` entry `{def_name}`"),
            );
        };
        self.project(&definition, path, Some(def_name.to_owned()), depth + 1)
    }

    fn project_object(
        &mut self,
        object: &Map<String, Value>,
        path: &mut Vec<String>,
        name: Option<String>,
        depth: usize,
    ) -> Ty {
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        if let Some(properties) = object.get("properties").and_then(Value::as_object) {
            for (property, schema) in properties {
                path.push(property.clone());
                let mut ty = self.project(schema, path, None, depth + 1);
                path.pop();
                if !required.contains(&property.as_str()) {
                    ty = ty.optional();
                }
                fields.push(FieldTy {
                    name: property.clone(),
                    ty,
                    declaration: None,
                });
            }
        }
        Ty::Record(Rc::new(RecordTy { name, fields }))
    }

    fn project_array(
        &mut self,
        object: &Map<String, Value>,
        path: &mut Vec<String>,
        depth: usize,
    ) -> Ty {
        // The projection can never manufacture `[T?]` (illegal since the
        // 2026-07-11 ruling): only object properties absent from `required`
        // wrap in Optional, and the one JSON spelling of an optional-ish
        // element — a null-admitting `items` type — is already refused above
        // as a null union. The element here is always a plain `T`.
        let element = match object.get("items") {
            Some(items) => {
                path.push("items".to_owned());
                let ty = self.project(items, path, None, depth + 1);
                path.pop();
                ty
            }
            None => Ty::Unknown,
        };
        Ty::List(Rc::new(element))
    }
}

fn project_string(object: &Map<String, Value>, name: Option<String>) -> Ty {
    match object.get("enum").and_then(Value::as_array) {
        Some(values) => {
            let variants = values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            Ty::Enum(Rc::new(EnumTy { name, variants }))
        }
        None => Ty::Str,
    }
}
