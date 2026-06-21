//! Type-derived input skeletons (`aion input <workflow_type>`).
//!
//! Triggering a workflow means handing the engine a JSON input document. Writing
//! that document from scratch against a schema is exactly the hand-mirroring the
//! authoring cluster exists to remove (C30, S14). This module derives a *valid*
//! input skeleton from the workflow's input schema — the same JSON-Schema
//! document the input codec is generated from — so the emitted document decodes
//! through that codec without a decode error, and is generated from the type,
//! never hand-written.
//!
//! The skeleton is the minimum structurally-valid document, not a populated one:
//! it carries no invented semantic defaults (ADR-001). Every *required* property
//! appears with a type-shaped placeholder — `""`, `0`, `0.0`, `false`, `[]`, a
//! nested object skeleton, or an enum's first wire value (the only value that
//! decodes) — and every *optional* property is omitted, since the generated
//! decoder reads an absent optional as `None`. A placeholder is a structural zero
//! the author replaces, never a decision made for them about what the value
//! should be.
//!
//! It walks the same supported v1 schema subset as [`super::schema`] and fails
//! loudly — naming the JSON pointer — for any construct outside it, so an input
//! skeleton is never silently emitted for a schema the codec generator would
//! reject.

use serde_json::{Map, Value};

use super::error::CodegenError;

/// Builds a structurally-valid input skeleton from a workflow `input_schema`
/// JSON-Schema document.
///
/// `schema_file` is the path the schema came from, used only to name the file in
/// a loud error. The returned [`Value`] decodes through the codec generated from
/// the same schema.
///
/// # Errors
///
/// Returns [`CodegenError::UnsupportedConstruct`] — naming the file and JSON
/// pointer — for any construct outside the supported v1 subset (`$ref`, `oneOf`,
/// `const`, `default`, open objects, type unions, non-string enums, …), matching
/// the codec generator's subset exactly.
pub fn build_input_skeleton(
    schema_file: &std::path::Path,
    schema: &Value,
) -> Result<Value, CodegenError> {
    let builder = SkeletonBuilder { schema_file };
    builder.walk(schema, "")
}

struct SkeletonBuilder<'a> {
    schema_file: &'a std::path::Path,
}

impl SkeletonBuilder<'_> {
    fn unsupported(&self, pointer: String, construct: String) -> CodegenError {
        CodegenError::UnsupportedConstruct {
            file: self.schema_file.to_path_buf(),
            pointer,
            construct,
        }
    }

    fn walk(&self, node: &Value, pointer: &str) -> Result<Value, CodegenError> {
        let Value::Object(entries) = node else {
            return Err(self.unsupported(
                pointer.to_owned(),
                "schema must be a JSON object".to_owned(),
            ));
        };

        // A string `enum`: the only decodable placeholder is one of its wire
        // values, so use the first — the minimum valid value, not a chosen
        // default among equals.
        if let Some(values) = entries.get("enum") {
            return self.walk_enum(values, pointer);
        }

        let Some(declared) = entries.get("type").and_then(Value::as_str) else {
            return Err(self.unsupported(
                pointer.to_owned(),
                "schema has neither a single-string `type` nor `enum`".to_owned(),
            ));
        };

        match declared {
            "string" => Ok(Value::String(String::new())),
            "integer" => Ok(Value::Number(0.into())),
            "number" => Ok(serde_json::json!(0.0)),
            "boolean" => Ok(Value::Bool(false)),
            // An `array` skeleton is the empty list: the minimum valid value,
            // carrying no invented element. (A non-empty list would be an
            // invented default element, ADR-001.)
            "array" => Ok(Value::Array(Vec::new())),
            "object" => self.walk_object(entries, pointer),
            other => Err(self.unsupported(
                join(pointer, "type"),
                format!("unsupported `type` value `{other}`"),
            )),
        }
    }

    fn walk_enum(&self, values: &Value, pointer: &str) -> Result<Value, CodegenError> {
        let enum_pointer = join(pointer, "enum");
        let Value::Array(values) = values else {
            return Err(self.unsupported(enum_pointer, "`enum` must be an array".to_owned()));
        };
        let Some(first) = values.first() else {
            return Err(self.unsupported(enum_pointer, "`enum` must not be empty".to_owned()));
        };
        let Some(wire) = first.as_str() else {
            return Err(self.unsupported(
                enum_pointer,
                "only string `enum` values are supported".to_owned(),
            ));
        };
        Ok(Value::String(wire.to_owned()))
    }

    fn walk_object(
        &self,
        entries: &Map<String, Value>,
        pointer: &str,
    ) -> Result<Value, CodegenError> {
        let Some(properties) = entries.get("properties") else {
            return Err(self.unsupported(
                pointer.to_owned(),
                "`object` schema without `properties`".to_owned(),
            ));
        };
        let Value::Object(properties) = properties else {
            return Err(self.unsupported(
                join(pointer, "properties"),
                "`properties` must be an object".to_owned(),
            ));
        };
        let required = required_names(entries);

        let properties_pointer = join(pointer, "properties");
        let mut skeleton = Map::new();
        for (property, child) in properties {
            // Optional properties are omitted: the generated decoder reads an
            // absent optional as `None`, so leaving it out is the no-default form.
            if !required.iter().any(|name| name == property) {
                continue;
            }
            let child_pointer = join(&properties_pointer, property);
            let value = self.walk(child, &child_pointer)?;
            skeleton.insert(property.clone(), value);
        }
        Ok(Value::Object(skeleton))
    }
}

/// Reads the `required` property names, treating an absent list as empty.
fn required_names(entries: &Map<String, Value>) -> Vec<String> {
    entries
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Joins a JSON pointer with a child segment.
fn join(pointer: &str, segment: &str) -> String {
    format!("{pointer}/{segment}")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::build_input_skeleton;
    use crate::codegen::error::CodegenError;

    fn skeleton(schema: &serde_json::Value) -> Result<serde_json::Value, CodegenError> {
        build_input_skeleton(Path::new("schemas/input.json"), schema)
    }

    #[test]
    fn required_scalars_get_type_shaped_placeholders() -> Result<(), Box<dyn std::error::Error>> {
        let schema = json!({
            "type": "object",
            "required": ["name", "count", "ratio", "active"],
            "additionalProperties": false,
            "properties": {
                "name": { "type": "string" },
                "count": { "type": "integer" },
                "ratio": { "type": "number" },
                "active": { "type": "boolean" },
                "note": { "type": "string" }
            }
        });
        let result = skeleton(&schema)?;
        // Required scalars present with zero-shaped placeholders; the optional
        // `note` omitted (no invented default).
        assert_eq!(
            result,
            json!({ "name": "", "count": 0, "ratio": 0.0, "active": false })
        );
        Ok(())
    }

    #[test]
    fn nested_required_objects_recurse() -> Result<(), Box<dyn std::error::Error>> {
        let schema = json!({
            "type": "object",
            "required": ["workspace"],
            "properties": {
                "workspace": {
                    "type": "object",
                    "required": ["path"],
                    "properties": { "path": { "type": "string" } }
                }
            }
        });
        assert_eq!(skeleton(&schema)?, json!({ "workspace": { "path": "" } }));
        Ok(())
    }

    #[test]
    fn required_enum_uses_the_first_wire_value() -> Result<(), Box<dyn std::error::Error>> {
        let schema = json!({
            "type": "object",
            "required": ["isolation"],
            "properties": {
                "isolation": { "type": "string", "enum": ["worktree", "copy", "vm"] }
            }
        });
        assert_eq!(skeleton(&schema)?, json!({ "isolation": "worktree" }));
        Ok(())
    }

    #[test]
    fn required_arrays_are_empty() -> Result<(), Box<dyn std::error::Error>> {
        let schema = json!({
            "type": "object",
            "required": ["tags"],
            "properties": {
                "tags": { "type": "array", "items": { "type": "string" } }
            }
        });
        assert_eq!(skeleton(&schema)?, json!({ "tags": [] }));
        Ok(())
    }

    #[test]
    fn unsupported_construct_fails_with_pointer() {
        let schema = json!({
            "type": "object",
            "required": ["w"],
            "properties": { "w": { "$ref": "#/$defs/w" } }
        });
        let result = skeleton(&schema);
        let Err(CodegenError::UnsupportedConstruct { pointer, .. }) = result else {
            unreachable!("expected UnsupportedConstruct, got {result:?}");
        };
        assert_eq!(pointer, "/properties/w");
    }

    #[test]
    fn empty_enum_fails() {
        let schema = json!({ "type": "string", "enum": [] });
        assert!(matches!(
            skeleton(&schema),
            Err(CodegenError::UnsupportedConstruct { .. })
        ));
    }
}
