//! Structured output of AWL-to-Gleam emission.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::types::{GType, NamedDef, TypeEnv};

/// One synthesized workflow entry implemented by the emitted Gleam module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SynthesizedWorkflowEntry {
    /// Reserved, deterministic routing identity used by parent spawns.
    pub workflow_type: String,
    /// Logical module containing the generated engine entry function.
    pub entry_module: String,
    /// Exported raw engine entry function.
    pub entry_function: String,
    /// JSON Schema accepted by the generated input codec.
    pub input_schema: Value,
    /// JSON Schema produced by the generated output codec.
    pub output_schema: Value,
    /// Workflow timeout inherited from the parent contract when the document
    /// declared one, `None` otherwise. `None` is the honest "no authored
    /// timeout" — the child arms no deadline rather than a buried default.
    pub timeout: Option<Duration>,
    /// Synthesized entries are package-internal implementation details.
    pub internal: bool,
}

/// Complete product of emission: source plus every package entry it implements.
#[derive(Clone, Debug, PartialEq)]
pub struct EmittedArtifact {
    /// Logical module implemented by the emitted source.
    pub entry_module: String,
    /// Complete Gleam module source.
    pub source: String,
    /// Synthesized same-package workflow entries in deterministic source order.
    pub synthesized_workflows: Vec<SynthesizedWorkflowEntry>,
}

impl EmittedArtifact {
    /// Stable JSON sidecar consumed by project packaging.
    #[must_use]
    pub fn project_metadata(&self) -> Value {
        let entries = self
            .synthesized_workflows
            .iter()
            .map(|entry| {
                json!({
                    "workflow_type": entry.workflow_type,
                    "entry_module": entry.entry_module,
                    "entry_function": entry.entry_function,
                    // Absent (authored no timeout) serialises as JSON null; the
                    // project sidecar reader decodes that back to `None`.
                    "timeout_seconds": entry.timeout.map(|timeout| timeout.as_secs()),
                    "input_schema": entry.input_schema,
                    "output_schema": entry.output_schema,
                    "internal": entry.internal,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "format_version": 1,
            "entry_module": self.entry_module,
            "synthesized_workflows": entries,
        })
    }
}

pub(super) fn schema_for_fields(env: &TypeEnv, fields: &[(String, GType)]) -> Value {
    schema_for_fields_inner(env, fields, &mut Vec::new())
}

fn schema_for_fields_inner(
    env: &TypeEnv,
    fields: &[(String, GType)],
    visiting: &mut Vec<String>,
) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (name, ty) in fields {
        properties.insert(name.clone(), schema_for_type_inner(env, ty, visiting));
        if !matches!(ty, GType::Option(_)) {
            required.push(Value::String(name.clone()));
        }
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

pub(super) fn schema_for_type(env: &TypeEnv, ty: &GType) -> Value {
    schema_for_type_inner(env, ty, &mut Vec::new())
}

fn schema_for_type_inner(env: &TypeEnv, ty: &GType, visiting: &mut Vec<String>) -> Value {
    match ty {
        GType::Bool => json!({ "type": "boolean" }),
        GType::Int | GType::Duration => json!({ "type": "integer" }),
        GType::Float => json!({ "type": "number" }),
        GType::Str => json!({ "type": "string" }),
        GType::Nil => json!({ "type": "null" }),
        GType::List(inner) => {
            json!({ "type": "array", "items": schema_for_type_inner(env, inner, visiting) })
        }
        GType::Option(inner) => {
            json!({ "anyOf": [schema_for_type_inner(env, inner, visiting), { "type": "null" }] })
        }
        GType::Unknown => json!({}),
        GType::Named(name) => {
            if visiting.iter().any(|active| active == name) {
                return json!({});
            }
            visiting.push(name.clone());
            let schema = match env.get(name) {
                Some(NamedDef::Alias(inner)) => schema_for_type_inner(env, inner, visiting),
                Some(NamedDef::Enum(variants)) => json!({ "type": "string", "enum": variants }),
                Some(NamedDef::Record(record)) => {
                    let fields = record
                        .fields
                        .iter()
                        .map(|field| (field.awl_name.clone(), field.ty.clone()))
                        .collect::<Vec<_>>();
                    schema_for_fields_inner(env, &fields, visiting)
                }
                None => json!({}),
            };
            visiting.pop();
            schema
        }
    }
}
