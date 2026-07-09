use serde_json::{Map, Value};
use thiserror::Error;

use crate::{Document, Span, TypeDecl, TypeRef, check};

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Failure to derive a JSON Schema from an AWL type declaration.
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
    /// An inline recursive record cannot be represented by this derivation.
    #[error("recursive type `{name}` cannot be derived")]
    RecursiveType {
        /// Span of the recursive type reference.
        span: Span,
        /// Recursive type name.
        name: String,
    },
}

impl SchemaError {
    /// Span that anchors a compiler-style diagnostic.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::UncleanDocument { span, .. }
            | Self::UnknownType { span, .. }
            | Self::RecursiveType { span, .. } => *span,
        }
    }
}

/// Derive JSON Schema draft 2020-12 for a checked AWL record declaration.
///
/// # Errors
///
/// Returns [`SchemaError`] if the document does not check cleanly, the named
/// declaration does not exist, or an inline recursive record blocks derivation.
pub fn schema_for_type(document: &Document, name: &str) -> Result<Value, SchemaError> {
    if let Some(error) = check(document).into_iter().next() {
        return Err(SchemaError::UncleanDocument {
            span: error.span,
            message: error.message,
        });
    }
    let declaration = find_type(document, name).ok_or_else(|| SchemaError::UnknownType {
        span: document.span,
        name: name.to_owned(),
    })?;
    let mut active = vec![name.to_owned()];
    record_schema(document, declaration, true, &mut active)
}

fn record_schema(
    document: &Document,
    declaration: &TypeDecl,
    include_draft: bool,
    active: &mut Vec<String>,
) -> Result<Value, SchemaError> {
    let mut schema = Map::new();
    if include_draft {
        schema.insert(
            "$schema".to_owned(),
            Value::String(DRAFT_2020_12.to_owned()),
        );
    }
    schema.insert("type".to_owned(), Value::String("object".to_owned()));
    if let Some(description) = &declaration.description {
        schema.insert("description".to_owned(), Value::String(description.clone()));
    }

    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in &declaration.fields {
        let (field_type, optional) = match &field.ty {
            TypeRef::Option { inner, .. } => (inner.as_ref(), true),
            other => (other, false),
        };
        let mut property = type_schema(document, field_type, active)?;
        if let Some(description) = &field.description {
            if let Some(object) = property.as_object_mut() {
                object.insert("description".to_owned(), Value::String(description.clone()));
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
    active: &mut Vec<String>,
) -> Result<Value, SchemaError> {
    match ty {
        TypeRef::Named { name, span } => match name.as_str() {
            "String" | "Dir" => Ok(primitive("string")),
            "Bool" => Ok(primitive("boolean")),
            "Int" => Ok(primitive("integer")),
            "Float" => Ok(primitive("number")),
            "Nil" => Ok(primitive("null")),
            _ => {
                if active.iter().any(|active_name| active_name == name) {
                    return Err(SchemaError::RecursiveType {
                        span: *span,
                        name: name.clone(),
                    });
                }
                let declaration =
                    find_type(document, name).ok_or_else(|| SchemaError::UnknownType {
                        span: *span,
                        name: name.clone(),
                    })?;
                active.push(name.clone());
                let result = record_schema(document, declaration, false, active);
                active.pop();
                result
            }
        },
        TypeRef::List { inner, .. } => {
            let mut schema = Map::new();
            schema.insert("type".to_owned(), Value::String("array".to_owned()));
            schema.insert("items".to_owned(), type_schema(document, inner, active)?);
            Ok(Value::Object(schema))
        }
        TypeRef::Option { inner, .. } => type_schema(document, inner, active),
    }
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
