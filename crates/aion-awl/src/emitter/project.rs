//! Record-shaped projection of JSON Schema documents onto Gleam types,
//! mirroring the checker's rules; structurally identical records unify with
//! already-registered ones because Gleam is nominal where the checker is
//! structural.

use serde_json::{Map, Value};

use crate::Span;

use super::error::EmitError;
use super::names::pascal;
use super::types::{FieldDef, GType, NamedDef, RecordDef, TypeEnv};

/// Structural JSON Schema keywords the projection refuses, by design
/// (mirrors the checker's list).
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

pub(super) struct Projector<'e> {
    env: &'e mut TypeEnv,
    defs: Map<String, Value>,
    span: Span,
    label: String,
}

impl<'e> Projector<'e> {
    pub(super) fn new(
        env: &'e mut TypeEnv,
        defs: Map<String, Value>,
        span: Span,
        label: String,
    ) -> Self {
        Self {
            env,
            defs,
            span,
            label,
        }
    }

    fn fail(&self, detail: &str) -> EmitError {
        EmitError::new(self.span, format!("schema for `{}`: {detail}", self.label))
    }

    pub(super) fn project(
        &mut self,
        schema: &Value,
        hint: &str,
        depth: usize,
    ) -> Result<GType, EmitError> {
        if depth > 24 {
            return Err(self.fail("the schema nests or self-references too deeply"));
        }
        let Some(object) = schema.as_object() else {
            return Err(self.fail("expected a schema object"));
        };
        for keyword in UNSUPPORTED_KEYWORDS {
            if object.contains_key(keyword) {
                return Err(self.fail(&format!(
                    "the record-shaped projection cannot honor the JSON Schema keyword \
                     `{keyword}`"
                )));
            }
        }
        if let Some(reference) = object.get("$ref") {
            let Some(target) = reference.as_str() else {
                return Err(self.fail("`$ref` must be a string"));
            };
            let Some(def_name) = target.strip_prefix("#/$defs/") else {
                return Err(self.fail(&format!(
                    "only `$defs`-local `$ref` targets are supported, found `{target}`"
                )));
            };
            let Some(definition) = self.defs.get(def_name).cloned() else {
                return Err(self.fail(&format!(
                    "`$ref` names a missing `$defs` entry `{def_name}`"
                )));
            };
            let def_hint = def_name.to_owned();
            return self.project(&definition, &def_hint, depth + 1);
        }
        match object.get("type") {
            Some(Value::String(kind)) => match kind.as_str() {
                "object" => self.project_object(object, hint, depth),
                "array" => self.project_array(object, hint, depth),
                "string" => Ok(GType::Str),
                "integer" => Ok(GType::Int),
                "number" => Ok(GType::Float),
                "boolean" => Ok(GType::Bool),
                other => Err(self.fail(&format!(
                    "`type` `{other}` cannot be projected onto a Gleam type"
                ))),
            },
            Some(Value::Array(_)) => Err(self.fail("union `type` arrays cannot be projected")),
            _ if object.contains_key("properties") => self.project_object(object, hint, depth),
            _ if object.contains_key("enum") => Ok(GType::Str),
            _ => Err(self.fail("a schema with no `type`, `properties`, or `enum` cannot be typed")),
        }
    }

    fn project_object(
        &mut self,
        object: &Map<String, Value>,
        hint: &str,
        depth: usize,
    ) -> Result<GType, EmitError> {
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        if let Some(properties) = object.get("properties").and_then(Value::as_object) {
            for (property, schema) in properties {
                let nested_hint = format!("{hint}{}", pascal(property));
                let mut ty = self.project(schema, &nested_hint, depth + 1)?;
                if !required.contains(&property.as_str()) && !matches!(ty, GType::Option(_)) {
                    ty = GType::Option(Box::new(ty));
                }
                fields.push(FieldDef {
                    awl_name: property.clone(),
                    ty,
                });
            }
        }
        Ok(self.intern_record(hint, fields))
    }

    fn project_array(
        &mut self,
        object: &Map<String, Value>,
        hint: &str,
        depth: usize,
    ) -> Result<GType, EmitError> {
        let Some(items) = object.get("items") else {
            return Err(self.fail("an array without `items` cannot be typed"));
        };
        let element_hint = format!("{hint}Item");
        let element = self.project(items, &element_hint, depth + 1)?;
        Ok(GType::List(Box::new(element)))
    }

    /// Register a projected record, reusing any structurally identical
    /// record already in the registry (order-insensitive field match).
    fn intern_record(&mut self, hint: &str, fields: Vec<FieldDef>) -> GType {
        // The door's root name is pre-claimed and always binds the decl name.
        if hint == self.label {
            self.env
                .defs
                .insert(hint.to_owned(), NamedDef::Record(RecordDef { fields }));
            return GType::Named(hint.to_owned());
        }
        let mut candidate: Vec<(&str, &GType)> = fields
            .iter()
            .map(|field| (field.awl_name.as_str(), &field.ty))
            .collect();
        candidate.sort_by_key(|(name, _)| *name);
        for (name, def) in &self.env.defs {
            let NamedDef::Record(record) = def else {
                continue;
            };
            let mut existing: Vec<(&str, &GType)> = record
                .fields
                .iter()
                .map(|field| (field.awl_name.as_str(), &field.ty))
                .collect();
            existing.sort_by_key(|(field, _)| *field);
            if existing == candidate {
                return GType::Named(name.clone());
            }
        }
        let name = self.env.names.fresh(&pascal(hint));
        self.env
            .defs
            .insert(name.clone(), NamedDef::Record(RecordDef { fields }));
        self.env.order.push(name.clone());
        GType::Named(name)
    }
}
