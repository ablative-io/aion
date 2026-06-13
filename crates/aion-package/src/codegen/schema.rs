//! JSON Schema → Gleam-type intermediate representation.
//!
//! Implements exactly the supported v1 subset and fails loudly — naming the
//! schema file and JSON pointer — for everything outside it:
//!
//! - `type: object` with `properties` (+ optional `required`,
//!   `additionalProperties: false`) → a Gleam record; nested objects become
//!   auxiliary records named by the property path.
//! - `type: string|integer|number|boolean` → `String|Int|Float|Bool`.
//! - `type: array` with `items` of a supported type → `List(t)`; the item
//!   type's name path gains an `item` segment.
//! - string `enum` → a custom type with one constructor per value, wire
//!   string preserved by the codecs.
//! - properties absent from `required` → `option.Option(t)`.
//!
//! Annotation and validation keywords that do not change the generated type
//! (`title`, `minLength`, `minimum`, ...) are accepted and ignored; the
//! packaging-side schema still enforces them at the dispatch boundary.
//! Everything else (`$ref`, `$defs`, `oneOf`, `const`, `default`, open
//! objects, ...) is a typed error.

use std::path::{Path, PathBuf};

use super::error::CodegenError;
use super::json::OrderedValue;
use super::names::{
    self, NameRegistry, fn_prefix, is_constructor_safe, is_reserved_word, is_snake_identifier,
    pointer_join, type_name,
};

/// Keywords that shape the generated type.
const STRUCTURAL_KEYWORDS: &[&str] = &[
    "type",
    "enum",
    "properties",
    "required",
    "additionalProperties",
    "items",
];

/// Annotation/validation keywords accepted and ignored: they constrain
/// values, not the generated type. (`default` is deliberately absent — a
/// schema default implies fill-in behaviour the generated codecs do not
/// have, so it fails loudly.)
const IGNORED_KEYWORDS: &[&str] = &[
    "$schema",
    "title",
    "description",
    "examples",
    "deprecated",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minItems",
    "maxItems",
    "uniqueItems",
];

/// A Gleam type reference in the generated module.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GleamType {
    /// `String`.
    String,
    /// `Int` (JSON `integer`).
    Int,
    /// `Float` (JSON `number`).
    Float,
    /// `Bool`.
    Bool,
    /// `List(inner)`.
    List(Box<GleamType>),
    /// A generated record or enum type.
    Named {
        /// The generated Gleam type name, e.g. `GateInputWorkspace`.
        type_name: String,
        /// The generated function prefix, e.g. `gate_input_workspace`.
        fn_prefix: String,
    },
}

/// One record field.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Field {
    /// JSON property name; also the Gleam record label.
    pub(crate) wire: String,
    /// Field type (wrapped in `option.Option` when not required).
    pub(crate) ty: GleamType,
    /// Whether the property is listed in `required`.
    pub(crate) required: bool,
}

/// A generated record type.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordDef {
    /// Generated type (and constructor) name.
    pub(crate) type_name: String,
    /// Generated codec function prefix.
    pub(crate) fn_prefix: String,
    /// JSON pointer of the defining object schema (empty = document root).
    pub(crate) pointer: String,
    /// Fields in schema property order.
    pub(crate) fields: Vec<Field>,
}

/// One enum constructor with its preserved wire string.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EnumVariant {
    /// Generated constructor name, e.g. `InputPlacementLocal`.
    pub(crate) constructor: String,
    /// The exact wire string from the schema `enum` list.
    pub(crate) wire: String,
}

/// A generated enum (custom type) from a string `enum` schema.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EnumDef {
    /// Generated type name.
    pub(crate) type_name: String,
    /// Generated codec function prefix.
    pub(crate) fn_prefix: String,
    /// JSON pointer of the defining enum schema.
    pub(crate) pointer: String,
    /// Variants in schema value order.
    pub(crate) variants: Vec<EnumVariant>,
}

/// A generated type definition.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TypeDef {
    /// A record from a `type: object` schema.
    Record(RecordDef),
    /// A custom type from a string `enum` schema.
    Enum(EnumDef),
}

/// Everything generated from one schema file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SchemaArtifact {
    /// Schema file, relative to the project root (`schemas/input.json`).
    pub(crate) file: PathBuf,
    /// Snake-case stem the root names derive from (`input`).
    pub(crate) stem: String,
    /// The document root type.
    pub(crate) root: GleamType,
    /// Type definitions in deterministic order (parents before children,
    /// children in property order).
    pub(crate) defs: Vec<TypeDef>,
}

/// Parses one schema document into its artifact, claiming every generated
/// name in `registry`.
pub(crate) fn parse_schema(
    file: &Path,
    stem: &str,
    document: &OrderedValue,
    registry: &mut NameRegistry,
) -> Result<SchemaArtifact, CodegenError> {
    if !is_snake_identifier(stem) || is_reserved_word(stem) {
        return Err(CodegenError::SchemaFileName {
            path: file.to_path_buf(),
            reason: format!(
                "stem `{stem}` must be a snake_case identifier \
                 (lowercase letters, digits, underscores) and not a Gleam reserved word"
            ),
        });
    }
    let mut walker = Walker {
        file,
        registry,
        defs: Vec::new(),
    };
    let segments = vec![stem.to_owned()];
    let root = walker.walk(document, "", &segments)?;
    if !matches!(root, GleamType::Named { .. }) {
        // Scalar/array roots emit `<stem>_to_json` / `<stem>_decoder`
        // wrappers; claim the root name so nothing else can collide with
        // that function pair.
        walker
            .registry
            .claim_type(&type_name(&segments), file, "")?;
    }
    Ok(SchemaArtifact {
        file: file.to_path_buf(),
        stem: stem.to_owned(),
        root,
        defs: walker.defs,
    })
}

struct Walker<'a> {
    file: &'a Path,
    registry: &'a mut NameRegistry,
    defs: Vec<TypeDef>,
}

impl Walker<'_> {
    fn unsupported(&self, pointer: String, construct: String) -> CodegenError {
        CodegenError::UnsupportedConstruct {
            file: self.file.to_path_buf(),
            pointer,
            construct,
        }
    }

    fn walk(
        &mut self,
        node: &OrderedValue,
        pointer: &str,
        segments: &[String],
    ) -> Result<GleamType, CodegenError> {
        let OrderedValue::Object(entries) = node else {
            return Err(self.unsupported(
                pointer.to_owned(),
                format!(
                    "schema must be a JSON object, found {} \
                     (boolean and non-object schemas are not supported)",
                    node.type_name()
                ),
            ));
        };
        for (key, _) in entries {
            let key = key.as_str();
            if !STRUCTURAL_KEYWORDS.contains(&key) && !IGNORED_KEYWORDS.contains(&key) {
                return Err(self.unsupported(
                    pointer_join(pointer, key),
                    format!("unrecognised keyword `{key}`"),
                ));
            }
        }
        let get = |key: &str| {
            entries
                .iter()
                .find(|(entry_key, _)| entry_key == key)
                .map(|(_, value)| value)
        };

        if let Some(values) = get("enum") {
            self.forbid(
                pointer,
                &get,
                &["properties", "required", "additionalProperties", "items"],
                "is not valid alongside `enum`",
            )?;
            if let Some(declared) = get("type")
                && declared.as_str() != Some("string")
            {
                return Err(self.unsupported(
                    pointer_join(pointer, "type"),
                    "only string `enum` schemas are supported".to_owned(),
                ));
            }
            return self.walk_enum(values, pointer, segments);
        }

        let Some(declared) = get("type") else {
            return Err(self.unsupported(
                pointer.to_owned(),
                "schema has neither `type` nor `enum`".to_owned(),
            ));
        };
        let Some(declared) = declared.as_str() else {
            return Err(self.unsupported(
                pointer_join(pointer, "type"),
                "`type` must be a single string (type unions are not supported)".to_owned(),
            ));
        };
        match declared {
            "object" => {
                self.forbid(pointer, &get, &["items"], "is only valid on `array`")?;
                self.walk_object(&get, pointer, segments)
            }
            "array" => {
                self.forbid(
                    pointer,
                    &get,
                    &["properties", "required", "additionalProperties"],
                    "is only valid on `object`",
                )?;
                let Some(items) = get("items") else {
                    return Err(self.unsupported(
                        pointer.to_owned(),
                        "`array` schema without `items`".to_owned(),
                    ));
                };
                let mut item_segments = segments.to_vec();
                item_segments.push("item".to_owned());
                let inner = self.walk(items, &pointer_join(pointer, "items"), &item_segments)?;
                Ok(GleamType::List(Box::new(inner)))
            }
            "string" | "integer" | "number" | "boolean" => {
                self.forbid(
                    pointer,
                    &get,
                    &["properties", "required", "additionalProperties"],
                    "is only valid on `object`",
                )?;
                self.forbid(pointer, &get, &["items"], "is only valid on `array`")?;
                Ok(match declared {
                    "string" => GleamType::String,
                    "integer" => GleamType::Int,
                    "number" => GleamType::Float,
                    _ => GleamType::Bool,
                })
            }
            other => Err(self.unsupported(
                pointer_join(pointer, "type"),
                format!(
                    "unsupported `type` value `{other}` (supported: object, array, \
                     string, integer, number, boolean, plus string `enum`)"
                ),
            )),
        }
    }

    fn forbid<'v>(
        &self,
        pointer: &str,
        get: &impl Fn(&str) -> Option<&'v OrderedValue>,
        keys: &[&str],
        reason: &str,
    ) -> Result<(), CodegenError> {
        for key in keys {
            if get(key).is_some() {
                return Err(
                    self.unsupported(pointer_join(pointer, key), format!("`{key}` {reason}"))
                );
            }
        }
        Ok(())
    }

    fn walk_enum(
        &mut self,
        values: &OrderedValue,
        pointer: &str,
        segments: &[String],
    ) -> Result<GleamType, CodegenError> {
        let enum_pointer = pointer_join(pointer, "enum");
        let OrderedValue::Array(values) = values else {
            return Err(self.unsupported(
                enum_pointer,
                format!("`enum` must be an array, found {}", values.type_name()),
            ));
        };
        if values.is_empty() {
            return Err(self.unsupported(enum_pointer, "`enum` must not be empty".to_owned()));
        }
        let name = type_name(segments);
        let prefix = fn_prefix(segments);
        self.registry.claim_type(&name, self.file, pointer)?;
        let mut variants: Vec<EnumVariant> = Vec::with_capacity(values.len());
        for value in values {
            let Some(wire) = value.as_str() else {
                return Err(self.unsupported(
                    enum_pointer,
                    format!(
                        "only string `enum` values are supported, found {}",
                        value.type_name()
                    ),
                ));
            };
            if variants.iter().any(|variant| variant.wire == wire) {
                return Err(
                    self.unsupported(enum_pointer, format!("duplicate `enum` value `{wire}`"))
                );
            }
            if !is_constructor_safe(wire) {
                return Err(self.unsupported(
                    enum_pointer,
                    format!(
                        "`enum` value `{wire}` cannot derive a Gleam constructor \
                         (letters and digits separated by `_` or `-`, starting with a letter)"
                    ),
                ));
            }
            let constructor = format!("{name}{}", names::pascal_case(wire));
            self.registry
                .claim_constructor(&constructor, self.file, pointer)?;
            variants.push(EnumVariant {
                constructor,
                wire: wire.to_owned(),
            });
        }
        self.defs.push(TypeDef::Enum(EnumDef {
            type_name: name.clone(),
            fn_prefix: prefix.clone(),
            pointer: pointer.to_owned(),
            variants,
        }));
        Ok(GleamType::Named {
            type_name: name,
            fn_prefix: prefix,
        })
    }

    fn walk_object<'v>(
        &mut self,
        get: &impl Fn(&str) -> Option<&'v OrderedValue>,
        pointer: &str,
        segments: &[String],
    ) -> Result<GleamType, CodegenError> {
        match get("additionalProperties") {
            None | Some(OrderedValue::Bool(false)) => {}
            Some(other) => {
                return Err(self.unsupported(
                    pointer_join(pointer, "additionalProperties"),
                    format!(
                        "`additionalProperties` must be `false` or absent \
                         (open objects are not representable as a Gleam record), found {}",
                        other.type_name()
                    ),
                ));
            }
        }
        let Some(properties) = get("properties") else {
            return Err(self.unsupported(
                pointer.to_owned(),
                "`object` schema without `properties`".to_owned(),
            ));
        };
        let OrderedValue::Object(properties) = properties else {
            return Err(self.unsupported(
                pointer_join(pointer, "properties"),
                format!(
                    "`properties` must be an object, found {}",
                    properties.type_name()
                ),
            ));
        };
        let required = self.required_names(get("required"), pointer, properties)?;

        let name = type_name(segments);
        let prefix = fn_prefix(segments);
        self.registry.claim_type(&name, self.file, pointer)?;
        self.registry.claim_constructor(&name, self.file, pointer)?;
        // Reserve this record's slot so it precedes its children in the
        // emitted module while children are walked depth-first.
        let slot = self.defs.len();

        let properties_pointer = pointer_join(pointer, "properties");
        let mut fields = Vec::with_capacity(properties.len());
        for (property, child) in properties {
            let property_pointer = pointer_join(&properties_pointer, property);
            if !is_snake_identifier(property) {
                return Err(self.unsupported(
                    property_pointer,
                    format!(
                        "property name `{property}` is not a valid Gleam record label \
                         (must match [a-z][a-z0-9_]*)"
                    ),
                ));
            }
            if is_reserved_word(property) {
                return Err(self.unsupported(
                    property_pointer,
                    format!("property name `{property}` is a Gleam reserved word"),
                ));
            }
            let mut child_segments = segments.to_vec();
            child_segments.push(property.clone());
            let ty = self.walk(child, &property_pointer, &child_segments)?;
            fields.push(Field {
                wire: property.clone(),
                ty,
                required: required.iter().any(|name| name == property),
            });
        }
        self.defs.insert(
            slot,
            TypeDef::Record(RecordDef {
                type_name: name.clone(),
                fn_prefix: prefix.clone(),
                pointer: pointer.to_owned(),
                fields,
            }),
        );
        Ok(GleamType::Named {
            type_name: name,
            fn_prefix: prefix,
        })
    }

    fn required_names(
        &self,
        required: Option<&OrderedValue>,
        pointer: &str,
        properties: &[(String, OrderedValue)],
    ) -> Result<Vec<String>, CodegenError> {
        let required_pointer = pointer_join(pointer, "required");
        let Some(required) = required else {
            return Ok(Vec::new());
        };
        let OrderedValue::Array(entries) = required else {
            return Err(self.unsupported(
                required_pointer,
                format!(
                    "`required` must be an array, found {}",
                    required.type_name()
                ),
            ));
        };
        let mut names = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(name) = entry.as_str() else {
                return Err(self.unsupported(
                    required_pointer,
                    format!(
                        "`required` entries must be strings, found {}",
                        entry.type_name()
                    ),
                ));
            };
            if names.iter().any(|existing| existing == name) {
                return Err(self.unsupported(
                    required_pointer,
                    format!("duplicate `required` entry `{name}`"),
                ));
            }
            if !properties.iter().any(|(property, _)| property == name) {
                return Err(self.unsupported(
                    required_pointer,
                    format!("`required` names `{name}`, which is not in `properties`"),
                ));
            }
            names.push(name.to_owned());
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Field, GleamType, SchemaArtifact, TypeDef, parse_schema};
    use crate::codegen::error::CodegenError;
    use crate::codegen::json::parse_ordered;
    use crate::codegen::names::NameRegistry;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn parse(stem: &str, json: &str) -> Result<SchemaArtifact, CodegenError> {
        let document = parse_ordered(json.as_bytes())
            .unwrap_or_else(|error| unreachable!("test schema must be valid JSON: {error}"));
        let mut registry = NameRegistry::default();
        parse_schema(
            Path::new(&format!("schemas/{stem}.json")),
            stem,
            &document,
            &mut registry,
        )
    }

    /// Asserts an `UnsupportedConstruct` with the exact pointer and a
    /// fragment of the construct description.
    fn assert_unsupported(
        stem: &str,
        json: &str,
        expected_pointer: &str,
        expected_fragment: &str,
    ) -> TestResult {
        let result = parse(stem, json);
        let Err(CodegenError::UnsupportedConstruct {
            file,
            pointer,
            construct,
        }) = result
        else {
            return Err(format!("expected UnsupportedConstruct, got {result:?}").into());
        };
        assert_eq!(file, Path::new(&format!("schemas/{stem}.json")));
        assert_eq!(pointer, expected_pointer, "construct: {construct}");
        assert!(
            construct.contains(expected_fragment),
            "construct `{construct}` must mention `{expected_fragment}`"
        );
        Ok(())
    }

    #[test]
    fn object_with_every_scalar_and_required_split() -> TestResult {
        let artifact = parse(
            "demo",
            r#"{
                "type": "object",
                "required": ["count", "ratio", "active"],
                "additionalProperties": false,
                "properties": {
                    "count": { "type": "integer" },
                    "ratio": { "type": "number" },
                    "active": { "type": "boolean" },
                    "note": { "type": "string" }
                }
            }"#,
        )?;

        let [TypeDef::Record(record)] = artifact.defs.as_slice() else {
            return Err(format!("expected one record, got {:?}", artifact.defs).into());
        };
        assert_eq!(record.type_name, "Demo");
        assert_eq!(record.fn_prefix, "demo");
        assert_eq!(
            record.fields,
            vec![
                Field {
                    wire: "count".to_owned(),
                    ty: GleamType::Int,
                    required: true,
                },
                Field {
                    wire: "ratio".to_owned(),
                    ty: GleamType::Float,
                    required: true,
                },
                Field {
                    wire: "active".to_owned(),
                    ty: GleamType::Bool,
                    required: true,
                },
                Field {
                    wire: "note".to_owned(),
                    ty: GleamType::String,
                    required: false,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn nested_objects_become_path_named_records_parent_first() -> TestResult {
        let artifact = parse(
            "gate_input",
            r#"{
                "type": "object",
                "required": ["workspace"],
                "properties": {
                    "workspace": {
                        "type": "object",
                        "required": ["path"],
                        "properties": { "path": { "type": "string" } }
                    }
                }
            }"#,
        )?;

        let names: Vec<&str> = artifact
            .defs
            .iter()
            .map(|def| match def {
                TypeDef::Record(record) => record.type_name.as_str(),
                TypeDef::Enum(definition) => definition.type_name.as_str(),
            })
            .collect();
        assert_eq!(names, vec!["GateInput", "GateInputWorkspace"]);
        let TypeDef::Record(parent) = &artifact.defs[0] else {
            return Err("expected parent record first".into());
        };
        assert_eq!(
            parent.fields[0].ty,
            GleamType::Named {
                type_name: "GateInputWorkspace".to_owned(),
                fn_prefix: "gate_input_workspace".to_owned(),
            }
        );
        let TypeDef::Record(child) = &artifact.defs[1] else {
            return Err("expected child record second".into());
        };
        assert_eq!(child.pointer, "/properties/workspace");
        Ok(())
    }

    #[test]
    fn string_enum_preserves_wire_strings_in_order() -> TestResult {
        let artifact = parse(
            "input",
            r#"{
                "type": "object",
                "required": ["isolation"],
                "properties": {
                    "isolation": {
                        "type": "string",
                        "enum": ["worktree", "copy", "overlay", "vm"]
                    }
                }
            }"#,
        )?;

        let Some(TypeDef::Enum(definition)) = artifact.defs.get(1) else {
            return Err(format!("expected enum def, got {:?}", artifact.defs).into());
        };
        assert_eq!(definition.type_name, "InputIsolation");
        let wires: Vec<&str> = definition
            .variants
            .iter()
            .map(|variant| variant.wire.as_str())
            .collect();
        assert_eq!(wires, vec!["worktree", "copy", "overlay", "vm"]);
        let constructors: Vec<&str> = definition
            .variants
            .iter()
            .map(|variant| variant.constructor.as_str())
            .collect();
        assert_eq!(
            constructors,
            vec![
                "InputIsolationWorktree",
                "InputIsolationCopy",
                "InputIsolationOverlay",
                "InputIsolationVm",
            ]
        );
        Ok(())
    }

    #[test]
    fn arrays_nest_and_item_objects_gain_an_item_segment() -> TestResult {
        let artifact = parse(
            "plan",
            r#"{
                "type": "object",
                "required": ["tags", "matrix", "steps"],
                "properties": {
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "matrix": {
                        "type": "array",
                        "items": { "type": "array", "items": { "type": "integer" } }
                    },
                    "steps": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["name"],
                            "properties": { "name": { "type": "string" } }
                        }
                    }
                }
            }"#,
        )?;

        let TypeDef::Record(record) = &artifact.defs[0] else {
            return Err("expected root record".into());
        };
        assert_eq!(
            record.fields[0].ty,
            GleamType::List(Box::new(GleamType::String))
        );
        assert_eq!(
            record.fields[1].ty,
            GleamType::List(Box::new(GleamType::List(Box::new(GleamType::Int))))
        );
        let GleamType::List(item) = &record.fields[2].ty else {
            return Err("steps must be a list".into());
        };
        assert_eq!(
            **item,
            GleamType::Named {
                type_name: "PlanStepsItem".to_owned(),
                fn_prefix: "plan_steps_item".to_owned(),
            }
        );
        let Some(TypeDef::Record(item_record)) = artifact.defs.get(1) else {
            return Err("expected item record def".into());
        };
        assert_eq!(item_record.pointer, "/properties/steps/items");
        Ok(())
    }

    #[test]
    fn scalar_root_claims_its_name() -> TestResult {
        let artifact = parse("output", r#"{ "type": "string" }"#)?;

        assert_eq!(artifact.root, GleamType::String);
        assert!(artifact.defs.is_empty());

        // The claimed root name collides with a same-named type elsewhere.
        let mut registry = NameRegistry::default();
        let document = parse_ordered(br#"{ "type": "string" }"#)?;
        parse_schema(
            Path::new("schemas/output.json"),
            "output",
            &document,
            &mut registry,
        )?;
        let other = parse_ordered(br#"{ "type": "object", "required": [], "properties": {} }"#)?;
        let result = parse_schema(
            Path::new("schemas/out.json"),
            "output",
            &other,
            &mut registry,
        );
        assert!(
            matches!(result, Err(CodegenError::NameCollision { ref name, .. }) if name == "Output"),
            "scalar root name must be claimed: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn ignored_validation_keywords_do_not_fail() -> TestResult {
        let artifact = parse(
            "input",
            r#"{
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "title": "input",
                "description": "annotated",
                "type": "object",
                "required": ["name", "cap"],
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string", "minLength": 1, "pattern": "^.+$" },
                    "cap": { "type": "integer", "minimum": 1, "maximum": 10 }
                }
            }"#,
        )?;

        assert_eq!(artifact.defs.len(), 1);
        Ok(())
    }

    #[test]
    fn dollar_defs_and_ref_fail_with_pointer() -> TestResult {
        assert_unsupported(
            "factored",
            r##"{
                "type": "object",
                "properties": { "workspace": { "$ref": "#/$defs/workspace" } },
                "$defs": { "workspace": { "type": "object", "properties": {} } }
            }"##,
            "/$defs",
            "unrecognised keyword `$defs`",
        )?;
        assert_unsupported(
            "refonly",
            r##"{
                "type": "object",
                "required": [],
                "properties": { "workspace": { "$ref": "#/elsewhere" } }
            }"##,
            "/properties/workspace/$ref",
            "unrecognised keyword `$ref`",
        )
    }

    #[test]
    fn one_of_const_and_default_fail_with_pointer() -> TestResult {
        assert_unsupported(
            "tagged",
            r#"{ "oneOf": [ { "type": "object", "properties": {} } ] }"#,
            "/oneOf",
            "unrecognised keyword `oneOf`",
        )?;
        assert_unsupported(
            "consted",
            r#"{ "type": "object", "required": [], "properties": { "tag": { "const": "x" } } }"#,
            "/properties/tag/const",
            "unrecognised keyword `const`",
        )?;
        assert_unsupported(
            "defaulted",
            r#"{
                "type": "object",
                "required": [],
                "properties": { "cap": { "type": "integer", "default": 3 } }
            }"#,
            "/properties/cap/default",
            "unrecognised keyword `default`",
        )
    }

    #[test]
    fn open_objects_and_type_unions_fail() -> TestResult {
        assert_unsupported(
            "open",
            r#"{ "type": "object", "additionalProperties": true, "properties": {} }"#,
            "/additionalProperties",
            "must be `false` or absent",
        )?;
        assert_unsupported(
            "unioned",
            r#"{ "type": ["string", "null"] }"#,
            "/type",
            "must be a single string",
        )?;
        assert_unsupported(
            "nulled",
            r#"{ "type": "null" }"#,
            "/type",
            "unsupported `type` value `null`",
        )
    }

    #[test]
    fn boolean_and_keywordless_schemas_fail_at_their_root() -> TestResult {
        assert_unsupported("boolish", "true", "", "schema must be a JSON object")?;
        assert_unsupported("bare", "{}", "", "neither `type` nor `enum`")?;
        assert_unsupported(
            "nested_bool",
            r#"{ "type": "object", "required": [], "properties": { "x": false } }"#,
            "/properties/x",
            "schema must be a JSON object",
        )
    }

    #[test]
    fn malformed_objects_fail_with_pointers() -> TestResult {
        assert_unsupported(
            "no_props",
            r#"{ "type": "object" }"#,
            "",
            "`object` schema without `properties`",
        )?;
        assert_unsupported(
            "ghost_required",
            r#"{ "type": "object", "required": ["ghost"], "properties": {} }"#,
            "/required",
            "`ghost`, which is not in `properties`",
        )?;
        assert_unsupported(
            "dup_required",
            r#"{
                "type": "object",
                "required": ["a", "a"],
                "properties": { "a": { "type": "string" } }
            }"#,
            "/required",
            "duplicate `required` entry `a`",
        )?;
        assert_unsupported(
            "items_on_object",
            r#"{ "type": "object", "properties": {}, "items": { "type": "string" } }"#,
            "/items",
            "`items` is only valid on `array`",
        )?;
        assert_unsupported(
            "props_on_string",
            r#"{ "type": "string", "properties": {} }"#,
            "/properties",
            "`properties` is only valid on `object`",
        )
    }

    #[test]
    fn malformed_arrays_and_enums_fail_with_pointers() -> TestResult {
        assert_unsupported(
            "itemless",
            r#"{ "type": "array" }"#,
            "",
            "`array` schema without `items`",
        )?;
        assert_unsupported(
            "empty_enum",
            r#"{ "type": "string", "enum": [] }"#,
            "/enum",
            "must not be empty",
        )?;
        assert_unsupported(
            "int_enum",
            r#"{ "enum": [1, 2] }"#,
            "/enum",
            "only string `enum` values",
        )?;
        assert_unsupported(
            "typed_enum",
            r#"{ "type": "integer", "enum": ["a"] }"#,
            "/type",
            "only string `enum` schemas",
        )?;
        assert_unsupported(
            "dup_enum",
            r#"{ "enum": ["a", "a"] }"#,
            "/enum",
            "duplicate `enum` value `a`",
        )?;
        assert_unsupported(
            "weird_enum",
            r#"{ "enum": ["1-fast"] }"#,
            "/enum",
            "cannot derive a Gleam constructor",
        )?;
        assert_unsupported(
            "enum_with_props",
            r#"{ "enum": ["a"], "properties": {} }"#,
            "/properties",
            "not valid alongside `enum`",
        )
    }

    #[test]
    fn unrepresentable_property_names_fail() -> TestResult {
        assert_unsupported(
            "reserved",
            r#"{ "type": "object", "required": [], "properties": { "type": { "type": "string" } } }"#,
            "/properties/type",
            "Gleam reserved word",
        )?;
        assert_unsupported(
            "kebab",
            r#"{ "type": "object", "required": [], "properties": { "a-b": { "type": "string" } } }"#,
            "/properties/a-b",
            "not a valid Gleam record label",
        )
    }

    #[test]
    fn bad_file_stems_are_rejected() {
        for stem in ["Input", "1st", "kebab-case", "use"] {
            let result = parse(stem, r#"{ "type": "string" }"#);
            assert!(
                matches!(result, Err(CodegenError::SchemaFileName { .. })),
                "stem `{stem}` must be rejected: {result:?}"
            );
        }
    }

    #[test]
    fn path_derived_names_collide_loudly_across_files() -> TestResult {
        let mut registry = NameRegistry::default();
        let first = parse_ordered(
            br#"{
                "type": "object",
                "required": [],
                "properties": {
                    "input": { "type": "object", "required": [], "properties": {} }
                }
            }"#,
        )?;
        parse_schema(
            Path::new("schemas/gate.json"),
            "gate",
            &first,
            &mut registry,
        )?;

        let second = parse_ordered(br#"{ "type": "object", "required": [], "properties": {} }"#)?;
        let result = parse_schema(
            Path::new("schemas/gate_input.json"),
            "gate_input",
            &second,
            &mut registry,
        );
        let Err(CodegenError::NameCollision {
            name,
            first_file,
            first_pointer,
            second_file,
            ..
        }) = result
        else {
            return Err(format!("expected NameCollision, got {result:?}").into());
        };
        assert_eq!(name, "GateInput");
        assert_eq!(first_file, Path::new("schemas/gate.json"));
        assert_eq!(first_pointer, "/properties/input");
        assert_eq!(second_file, Path::new("schemas/gate_input.json"));
        Ok(())
    }
}
