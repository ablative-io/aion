use serde_json::{Map, Value};
use thiserror::Error;

use crate::{Document, Span, TypeDecl, TypeRef, check};

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Failure to derive a JSON Schema from an AWL contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchemaError {
    /// The supplied document has a checker error and is not safe to derive.
    #[error("document does not check cleanly: {message}")]
    UncleanDocument {
        /// Span of the first checker error.
        span: Span,
        /// Checker error message.
        message: String,
    },
    /// The requested declaration does not exist.
    #[error("unknown type `{name}`")]
    UnknownType {
        /// Document span used to anchor the diagnostic.
        span: Span,
        /// Requested type name.
        name: String,
    },
    /// The workflow has no output contract to inspect.
    #[error("workflow has no output contract")]
    MissingOutput {
        /// Document span used to anchor the diagnostic.
        span: Span,
    },
}

impl SchemaError {
    /// Span that anchors a compiler-style diagnostic.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::UncleanDocument { span, .. }
            | Self::UnknownType { span, .. }
            | Self::MissingOutput { span } => *span,
        }
    }
}

/// Derive JSON Schema draft 2020-12 for a checked AWL record declaration.
///
/// # Errors
///
/// Returns [`SchemaError`] if the document is unclean or the type is unknown.
pub fn schema_for_type(document: &Document, name: &str) -> Result<Value, SchemaError> {
    ensure_clean(document)?;
    let declaration = find_type(document, name).ok_or_else(|| SchemaError::UnknownType {
        span: document.span,
        name: name.to_owned(),
    })?;
    let mut schema = record_schema(document, declaration, Some(name))?;
    add_draft_and_defs(document, &mut schema, Some(name))?;
    Ok(schema)
}

/// Derive one inspection schema containing the workflow input and output contracts.
///
/// # Errors
///
/// Returns [`SchemaError`] if the document is unclean or has no output.
pub fn schema_for_workflow(document: &Document) -> Result<Value, SchemaError> {
    ensure_clean(document)?;
    let output = document.output.as_ref().ok_or(SchemaError::MissingOutput {
        span: document.span,
    })?;
    let mut input_properties = Map::new();
    let mut input_required = Vec::new();
    for input in &document.inputs {
        let (ty, optional) = unwrap_option(&input.ty);
        input_properties.insert(input.name.clone(), type_schema(document, ty, None)?);
        if !optional {
            input_required.push(Value::String(input.name.clone()));
        }
    }
    let mut input_schema = Map::new();
    input_schema.insert("type".to_owned(), Value::String("object".to_owned()));
    input_schema.insert("properties".to_owned(), Value::Object(input_properties));
    input_schema.insert("required".to_owned(), Value::Array(input_required));

    let mut properties = Map::new();
    properties.insert("input".to_owned(), Value::Object(input_schema));
    properties.insert(
        "output".to_owned(),
        type_schema(document, unwrap_option(&output.ty).0, None)?,
    );
    let mut schema = Map::new();
    schema.insert("type".to_owned(), Value::String("object".to_owned()));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert(
        "required".to_owned(),
        Value::Array(vec![
            Value::String("input".to_owned()),
            Value::String("output".to_owned()),
        ]),
    );
    let mut value = Value::Object(schema);
    add_draft_and_defs(document, &mut value, None)?;
    Ok(value)
}

fn ensure_clean(document: &Document) -> Result<(), SchemaError> {
    if let Some(error) = check(document).into_iter().next() {
        Err(SchemaError::UncleanDocument {
            span: error.span,
            message: error.message,
        })
    } else {
        Ok(())
    }
}

fn add_draft_and_defs(
    document: &Document,
    schema: &mut Value,
    root: Option<&str>,
) -> Result<(), SchemaError> {
    let object = schema.as_object_mut().ok_or(SchemaError::UnknownType {
        span: document.span,
        name: root.unwrap_or("workflow").to_owned(),
    })?;
    object.insert(
        "$schema".to_owned(),
        Value::String(DRAFT_2020_12.to_owned()),
    );
    let names = reachable_types(document, root);
    let mut definitions = Map::new();
    for name in names {
        let declaration = find_type(document, &name).ok_or_else(|| SchemaError::UnknownType {
            span: document.span,
            name: name.clone(),
        })?;
        definitions.insert(name, record_schema(document, declaration, root)?);
    }
    if !definitions.is_empty() {
        object.insert("$defs".to_owned(), Value::Object(definitions));
    }
    Ok(())
}

fn record_schema(
    document: &Document,
    declaration: &TypeDecl,
    root: Option<&str>,
) -> Result<Value, SchemaError> {
    let mut schema = Map::new();
    schema.insert("type".to_owned(), Value::String("object".to_owned()));
    if let Some(description) = semantic_description(declaration.description.as_deref()) {
        schema.insert("description".to_owned(), Value::String(description));
    }
    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in &declaration.fields {
        let (field_type, optional) = unwrap_option(&field.ty);
        let mut property = type_schema(document, field_type, root)?;
        if let Some(description) = semantic_description(field.description.as_deref()) {
            if let Some(object) = property.as_object_mut() {
                object.insert("description".to_owned(), Value::String(description));
            }
        }
        properties.insert(field.name.clone(), property);
        if !optional {
            required.push(Value::String(field.name.clone()));
        }
    }
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), Value::Array(required));
    Ok(Value::Object(schema))
}

fn type_schema(
    document: &Document,
    ty: &TypeRef,
    root: Option<&str>,
) -> Result<Value, SchemaError> {
    match ty {
        TypeRef::Named { name, span } => match name.as_str() {
            "String" | "Dir" => Ok(primitive("string")),
            "Bool" => Ok(primitive("boolean")),
            "Int" => Ok(primitive("integer")),
            "Float" => Ok(primitive("number")),
            "Nil" => Ok(primitive("null")),
            _ => {
                if find_type(document, name).is_none() {
                    return Err(SchemaError::UnknownType {
                        span: *span,
                        name: name.clone(),
                    });
                }
                let reference = if root == Some(name.as_str()) {
                    "#".to_owned()
                } else {
                    format!("#/$defs/{name}")
                };
                let mut schema = Map::new();
                schema.insert("$ref".to_owned(), Value::String(reference));
                Ok(Value::Object(schema))
            }
        },
        TypeRef::List { inner, .. } => {
            let mut schema = Map::new();
            schema.insert("type".to_owned(), Value::String("array".to_owned()));
            schema.insert("items".to_owned(), type_schema(document, inner, root)?);
            Ok(Value::Object(schema))
        }
        TypeRef::Option { inner, .. } => type_schema(document, inner, root),
    }
}

fn reachable_types(document: &Document, root: Option<&str>) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(root_name) = root {
        if let Some(declaration) = find_type(document, root_name) {
            for field in &declaration.fields {
                collect_type_names(&field.ty, root, &mut names);
            }
        }
    } else {
        for input in &document.inputs {
            collect_type_names(&input.ty, None, &mut names);
        }
        if let Some(output) = &document.output {
            collect_type_names(&output.ty, None, &mut names);
        }
    }
    let mut index = 0;
    while index < names.len() {
        if let Some(declaration) = find_type(document, &names[index]) {
            for field in &declaration.fields {
                collect_type_names(&field.ty, root, &mut names);
            }
        }
        index += 1;
    }
    names
}

fn collect_type_names(ty: &TypeRef, root: Option<&str>, names: &mut Vec<String>) {
    match ty {
        TypeRef::Named { name, .. }
            if !matches!(
                name.as_str(),
                "String" | "Dir" | "Bool" | "Int" | "Float" | "Nil"
            ) && root != Some(name.as_str())
                && !names.contains(name) =>
        {
            names.push(name.clone());
        }
        TypeRef::List { inner, .. } | TypeRef::Option { inner, .. } => {
            collect_type_names(inner, root, names);
        }
        TypeRef::Named { .. } => {}
    }
}

fn unwrap_option(ty: &TypeRef) -> (&TypeRef, bool) {
    match ty {
        TypeRef::Option { inner, .. } => (inner, true),
        other => (other, false),
    }
}

fn semantic_description(source: Option<&str>) -> Option<String> {
    source.map(|description| {
        description
            .split('\n')
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn primitive(name: &str) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_owned(), Value::String(name.to_owned()));
    Value::Object(schema)
}

fn find_type<'a>(document: &'a Document, name: &str) -> Option<&'a TypeDecl> {
    document
        .types
        .iter()
        .find(|declaration| declaration.name == name)
}
