//! JSON Schema (draft 2020-12) derivation from AWL type declarations — the
//! one pure public derivation every consumer shares (start forms, worker
//! contracts, model output contracts).
//!
//! Shorthand records and enums derive structurally; `///` doc lines flow to
//! `description`s; `?` maps to "not in `required`". Schema-door types
//! (inline `schema { … }` and `schema("file")`) re-emit their source JSON
//! verbatim, so constraint keywords ride through untouched.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::ast::{Document, TypeBody, TypeDecl, TypeRef, doc_text};

use super::error::SchemaError;

/// Derive the JSON Schema for a named declared type, with schema imports
/// unresolvable (no document directory). Prefer [`schema_for_type_in`] when
/// the `.awl` file's directory is known.
///
/// # Errors
///
/// Returns [`SchemaError`] when the type is undeclared, or when its schema
/// door cannot be resolved or parsed.
pub fn schema_for_type(document: &Document, name: &str) -> Result<Value, SchemaError> {
    Deriver::new(document, None).type_schema(name)
}

/// Derive the JSON Schema for a named declared type, resolving schema
/// imports relative to `root` (the document's directory).
///
/// # Errors
///
/// Returns [`SchemaError`] when the type is undeclared, or when its schema
/// door cannot be resolved or parsed.
pub fn schema_for_type_in(
    document: &Document,
    root: &Path,
    name: &str,
) -> Result<Value, SchemaError> {
    Deriver::new(document, Some(root)).type_schema(name)
}

/// Derive the workflow's start contract: one object schema over its inputs,
/// `?`-typed inputs omitted from `required`, narration as the description.
///
/// # Errors
///
/// Returns [`SchemaError`] when an input's schema door cannot be resolved
/// or parsed.
pub fn schema_for_workflow(document: &Document) -> Result<Value, SchemaError> {
    Deriver::new(document, None).workflow_schema()
}

/// Derive the workflow's start contract, resolving schema imports relative
/// to `root` (the document's directory).
///
/// # Errors
///
/// Returns [`SchemaError`] when an input's schema door cannot be resolved
/// or parsed.
pub fn schema_for_workflow_in(document: &Document, root: &Path) -> Result<Value, SchemaError> {
    Deriver::new(document, Some(root)).workflow_schema()
}

/// Derive the workflow's outcome envelope, with schema imports unresolvable
/// (no document directory). Prefer [`schema_for_outcomes_in`] when the
/// `.awl` file's directory is known.
///
/// # Errors
///
/// Returns [`SchemaError`] when an outcome payload type is undeclared, or
/// when its schema door cannot be resolved or parsed.
pub fn schema_for_outcomes(document: &Document) -> Result<Value, SchemaError> {
    Deriver::new(document, None).envelope_schema()
}

/// Derive the workflow's outcome envelope — the `{ "outcome": name,
/// "payload": … }` object the emitted codec produces — resolving schema
/// imports relative to `root` (the document's directory). A single-outcome
/// workflow pins `outcome` with `const`; multiple outcomes become `oneOf`
/// arms pairing each outcome name with its payload schema.
///
/// # Errors
///
/// Returns [`SchemaError`] when an outcome payload type is undeclared, or
/// when its schema door cannot be resolved or parsed.
pub fn schema_for_outcomes_in(document: &Document, root: &Path) -> Result<Value, SchemaError> {
    Deriver::new(document, Some(root)).envelope_schema()
}

struct Deriver<'a> {
    document: &'a Document,
    root: Option<&'a Path>,
}

impl<'a> Deriver<'a> {
    const fn new(document: &'a Document, root: Option<&'a Path>) -> Self {
        Self { document, root }
    }

    fn decl(&self, name: &str) -> Option<&'a TypeDecl> {
        self.document.types.iter().find(|decl| decl.name == name)
    }

    fn type_schema(&self, name: &str) -> Result<Value, SchemaError> {
        if let Some(schema) = builtin_schema(name) {
            return Ok(schema);
        }
        let Some(decl) = self.decl(name) else {
            return Err(SchemaError::UnknownType {
                name: name.to_owned(),
                span: self.document.name_span,
            });
        };
        let mut defs = Map::new();
        let mut visited = BTreeSet::new();
        visited.insert(name.to_owned());
        let mut schema = self.decl_schema(decl, name, &mut defs, &mut visited)?;
        if !defs.is_empty()
            && let Some(object) = schema.as_object_mut()
        {
            object.insert("$defs".to_owned(), Value::Object(defs));
        }
        Ok(schema)
    }

    fn workflow_schema(&self) -> Result<Value, SchemaError> {
        let mut defs = Map::new();
        let mut properties = Map::new();
        let mut required = Vec::new();
        for input in &self.document.inputs {
            let mut visited = BTreeSet::new();
            let schema = self.type_ref_schema(&input.ty, "", &mut defs, &mut visited)?;
            if !matches!(input.ty, TypeRef::Optional { .. }) {
                required.push(Value::String(input.name.clone()));
            }
            properties.insert(input.name.clone(), schema);
        }
        let mut object = Map::new();
        object.insert("type".to_owned(), json!("object"));
        let narration = doc_text(&self.document.narration);
        if !narration.is_empty() {
            object.insert("description".to_owned(), Value::String(narration));
        }
        object.insert("properties".to_owned(), Value::Object(properties));
        object.insert("required".to_owned(), Value::Array(required));
        if !defs.is_empty() {
            object.insert("$defs".to_owned(), Value::Object(defs));
        }
        Ok(Value::Object(object))
    }

    fn envelope_schema(&self) -> Result<Value, SchemaError> {
        let mut defs = Map::new();
        let mut arms = Vec::new();
        for outcome in &self.document.outcomes {
            let payload = self.outcome_payload_schema(&outcome.ty, &mut defs)?;
            arms.push((outcome.name.clone(), payload));
        }
        let mut object = Map::new();
        object.insert(
            "$schema".to_owned(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        object.insert(
            "description".to_owned(),
            Value::String(format!(
                "The {} outcome envelope: which declared workflow outcome fired, \
                 with its payload.",
                self.document.name
            )),
        );
        object.insert("type".to_owned(), json!("object"));
        object.insert("required".to_owned(), json!(["outcome", "payload"]));
        if arms.len() == 1 {
            let mut properties = Map::new();
            for (name, payload) in arms {
                properties.insert("outcome".to_owned(), json!({ "const": name }));
                properties.insert("payload".to_owned(), payload);
            }
            object.insert("properties".to_owned(), Value::Object(properties));
        } else {
            let branches: Vec<Value> = arms
                .into_iter()
                .map(|(name, payload)| {
                    json!({
                        "properties": {
                            "outcome": { "const": name },
                            "payload": payload,
                        }
                    })
                })
                .collect();
            object.insert("oneOf".to_owned(), Value::Array(branches));
        }
        if !defs.is_empty() {
            object.insert("$defs".to_owned(), Value::Object(defs));
        }
        Ok(Value::Object(object))
    }

    /// An outcome payload's schema, inlined like the reference envelope. A
    /// named payload type inlines its body; a payload whose inlining would
    /// produce a document-root `"#"` self-reference (which would resolve to
    /// the envelope, not the payload) derives through `$defs` instead.
    fn outcome_payload_schema(
        &self,
        ty: &TypeRef,
        defs: &mut Map<String, Value>,
    ) -> Result<Value, SchemaError> {
        if let TypeRef::Named { name, .. } = ty
            && builtin_schema(name).is_none()
            && let Some(decl) = self.decl(name)
        {
            let mut trial = defs.clone();
            let mut visited = BTreeSet::new();
            visited.insert(name.clone());
            let inline = self.decl_schema(decl, name, &mut trial, &mut visited)?;
            if !contains_root_ref(&inline) && !trial.values().any(contains_root_ref) {
                *defs = trial;
                return Ok(inline);
            }
        }
        let mut visited = BTreeSet::new();
        self.type_ref_schema(ty, "", defs, &mut visited)
    }

    /// The schema of one declaration's body, with `root_name` the type the
    /// emitted document is rooted at (self-references become `"#"`).
    fn decl_schema(
        &self,
        decl: &TypeDecl,
        root_name: &str,
        defs: &mut Map<String, Value>,
        visited: &mut BTreeSet<String>,
    ) -> Result<Value, SchemaError> {
        match &decl.body {
            TypeBody::Record { fields } => {
                let mut properties = Map::new();
                let mut required = Vec::new();
                for field in fields {
                    let mut schema = self.type_ref_schema(&field.ty, root_name, defs, visited)?;
                    let docs = doc_text(&field.docs);
                    if !docs.is_empty()
                        && let Some(object) = schema.as_object_mut()
                    {
                        object.insert("description".to_owned(), Value::String(docs));
                    }
                    if !matches!(field.ty, TypeRef::Optional { .. }) {
                        required.push(Value::String(field.name.clone()));
                    }
                    properties.insert(field.name.clone(), schema);
                }
                let mut object = Map::new();
                object.insert("type".to_owned(), json!("object"));
                let docs = doc_text(&decl.docs);
                if !docs.is_empty() {
                    object.insert("description".to_owned(), Value::String(docs));
                }
                object.insert("properties".to_owned(), Value::Object(properties));
                object.insert("required".to_owned(), Value::Array(required));
                Ok(Value::Object(object))
            }
            TypeBody::Enum { variants } => {
                let mut object = Map::new();
                object.insert("type".to_owned(), json!("string"));
                let docs = doc_text(&decl.docs);
                if !docs.is_empty() {
                    object.insert("description".to_owned(), Value::String(docs));
                }
                object.insert(
                    "enum".to_owned(),
                    Value::Array(
                        variants
                            .iter()
                            .map(|variant| Value::String(variant.name.clone()))
                            .collect(),
                    ),
                );
                Ok(Value::Object(object))
            }
            TypeBody::SchemaInline { body, .. } => {
                serde_json::from_str(body).map_err(|error| SchemaError::InvalidJson {
                    name: decl.name.clone(),
                    detail: error.to_string(),
                    span: decl.name_span,
                })
            }
            TypeBody::SchemaImport { path, path_span } => {
                let Some(root) = self.root else {
                    return Err(SchemaError::ImportUnresolved {
                        path: path.clone(),
                        span: *path_span,
                    });
                };
                let text = std::fs::read_to_string(root.join(path)).map_err(|error| {
                    SchemaError::ImportUnreadable {
                        path: path.clone(),
                        detail: error.to_string(),
                        span: *path_span,
                    }
                })?;
                serde_json::from_str(&text).map_err(|error| SchemaError::InvalidJson {
                    name: decl.name.clone(),
                    detail: error.to_string(),
                    span: *path_span,
                })
            }
        }
    }

    fn type_ref_schema(
        &self,
        type_ref: &TypeRef,
        root_name: &str,
        defs: &mut Map<String, Value>,
        visited: &mut BTreeSet<String>,
    ) -> Result<Value, SchemaError> {
        match type_ref {
            TypeRef::Named { name, span } => {
                if let Some(schema) = builtin_schema(name) {
                    return Ok(schema);
                }
                if name == root_name {
                    return Ok(json!({ "$ref": "#" }));
                }
                let Some(decl) = self.decl(name) else {
                    return Err(SchemaError::UnknownType {
                        name: name.clone(),
                        span: *span,
                    });
                };
                if visited.insert(name.clone()) {
                    let schema = self.decl_schema(decl, root_name, defs, visited)?;
                    defs.insert(name.clone(), schema);
                }
                Ok(json!({ "$ref": format!("#/$defs/{name}") }))
            }
            TypeRef::List { inner, .. } => {
                let items = self.type_ref_schema(inner, root_name, defs, visited)?;
                Ok(json!({ "type": "array", "items": items }))
            }
            TypeRef::Optional { inner, .. } => {
                // Optionality is the field's membership in `required`; the
                // value schema is the inner type's — never nullable.
                self.type_ref_schema(inner, root_name, defs, visited)
            }
        }
    }
}

/// Whether a schema tree carries a document-root `{"$ref": "#"}` reference.
fn contains_root_ref(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, inner)| {
            (key == "$ref" && inner.as_str() == Some("#")) || contains_root_ref(inner)
        }),
        Value::Array(items) => items.iter().any(contains_root_ref),
        _ => false,
    }
}

fn builtin_schema(name: &str) -> Option<Value> {
    match name {
        "Bool" => Some(json!({ "type": "boolean" })),
        "Int" => Some(json!({ "type": "integer" })),
        "Float" => Some(json!({ "type": "number" })),
        "String" | "Dir" => Some(json!({ "type": "string" })),
        "Nil" => Some(json!({ "type": "null" })),
        _ => None,
    }
}
