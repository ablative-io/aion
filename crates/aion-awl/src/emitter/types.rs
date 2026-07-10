//! The emitter's type environment: declared shorthand records and enums plus
//! record-shaped projections of both schema doors, unified into one set of
//! nominal Gleam types.
//!
//! Projection mirrors the checker's rules (`object`/`properties`/`required`,
//! nested objects, arrays, string enums, `$defs`-local `$ref`). Because Gleam
//! is nominal where the checker is structural, a projected record that is
//! structurally identical to an already-registered record reuses that type
//! instead of synthesizing a twin (the flagship's `$defs` `Lens` lands on the
//! declared `Lens`). Projected string enums type as Gleam `String` — the
//! checker compares them with string literals, and a nominal Gleam enum
//! could not.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::Span;
use crate::ast::{Document, TypeBody, TypeRef};

use super::error::EmitError;
use super::names::{UpperNames, snake};

/// A Gleam-facing type in the emitter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GType {
    Bool,
    Int,
    Float,
    Str,
    Nil,
    Duration,
    List(Box<GType>),
    Option(Box<GType>),
    /// A registered named definition (record, enum, or alias).
    Named(String),
    /// Only produced by expression inference (empty list literals); never
    /// registered in a definition.
    Unknown,
}

/// One field of a registered record definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FieldDef {
    /// Wire (AWL/JSON) field name.
    pub(super) awl_name: String,
    /// Field type; optionality is `GType::Option`.
    pub(super) ty: GType,
}

/// A registered record definition (constructor name = type name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecordDef {
    pub(super) fields: Vec<FieldDef>,
}

/// A registered named definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NamedDef {
    Record(RecordDef),
    /// Payload-less enum with its variant constructors.
    Enum(Vec<String>),
    /// A schema-door root that projected to a non-record type.
    Alias(GType),
}

/// The emitter's registry of named Gleam types.
#[derive(Debug, Default)]
pub(super) struct TypeEnv {
    pub(super) defs: BTreeMap<String, NamedDef>,
    /// Emission order: declared names in document order, synthesized names
    /// appended as projection creates them.
    pub(super) order: Vec<String>,
    pub(super) names: UpperNames,
}

impl TypeEnv {
    pub(super) fn get(&self, name: &str) -> Option<&NamedDef> {
        self.defs.get(name)
    }

    /// Resolve alias chains to a structural `GType`.
    pub(super) fn resolve(&self, ty: &GType) -> GType {
        let mut current = ty.clone();
        for _ in 0..16 {
            match current {
                GType::Named(ref name) => match self.defs.get(name) {
                    Some(NamedDef::Alias(inner)) => current = inner.clone(),
                    _ => return current,
                },
                other => return other,
            }
        }
        current
    }

    /// Resolve to a record definition, when the type is one.
    pub(super) fn record_of(&self, ty: &GType) -> Option<(String, &RecordDef)> {
        match self.resolve(ty) {
            GType::Named(name) => match self.defs.get(&name) {
                Some(NamedDef::Record(record)) => Some((name, record)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Render the Gleam type expression for a `GType`.
    pub(super) fn gleam_type(&self, ty: &GType) -> String {
        match ty {
            GType::Bool => "Bool".to_owned(),
            GType::Int => "Int".to_owned(),
            GType::Float => "Float".to_owned(),
            GType::Str => "String".to_owned(),
            GType::Nil | GType::Unknown => "Nil".to_owned(),
            GType::Duration => "duration.Duration".to_owned(),
            GType::List(inner) => format!("List({})", self.gleam_type(inner)),
            GType::Option(inner) => format!("Option({})", self.gleam_type(inner)),
            GType::Named(name) => match self.defs.get(name) {
                Some(NamedDef::Alias(inner)) => self.gleam_type(inner),
                _ => name.clone(),
            },
        }
    }

    /// The codec-function stem for a type (`x` in `x_codec`/`x_to_json`).
    pub(super) fn codec_name(&self, ty: &GType) -> String {
        match ty {
            GType::Bool => "bool".to_owned(),
            GType::Int => "int".to_owned(),
            GType::Float => "float".to_owned(),
            GType::Str => "string".to_owned(),
            GType::Nil | GType::Unknown => "nil".to_owned(),
            GType::Duration => "duration_ms".to_owned(),
            GType::List(inner) => format!("list_{}", self.codec_name(inner)),
            GType::Option(inner) => format!("option_{}", self.codec_name(inner)),
            GType::Named(name) => match self.defs.get(name) {
                Some(NamedDef::Alias(inner)) => self.codec_name(inner),
                _ => snake(name),
            },
        }
    }

    /// A zero value of `ty`, used as the decoder-failure default.
    pub(super) fn zero_expr(&self, ty: &GType, span: Span) -> Result<String, EmitError> {
        self.zero_expr_in(ty, span, &mut Vec::new())
    }

    fn zero_expr_in(
        &self,
        ty: &GType,
        span: Span,
        visiting: &mut Vec<String>,
    ) -> Result<String, EmitError> {
        match ty {
            GType::Bool => Ok("False".to_owned()),
            GType::Int => Ok("0".to_owned()),
            GType::Float => Ok("0.0".to_owned()),
            GType::Str => Ok("\"\"".to_owned()),
            GType::Nil | GType::Unknown => Ok("Nil".to_owned()),
            GType::Duration => Ok("duration.milliseconds(0)".to_owned()),
            GType::List(_) => Ok("[]".to_owned()),
            GType::Option(_) => Ok("None".to_owned()),
            GType::Named(name) => {
                if visiting.iter().any(|seen| seen == name) {
                    return Err(EmitError::new(
                        span,
                        format!(
                            "type `{name}` recurses through required fields, so no default \
                             value exists for the generated decoder"
                        ),
                    ));
                }
                visiting.push(name.clone());
                let rendered = match self.defs.get(name) {
                    Some(NamedDef::Record(record)) => {
                        if record.fields.is_empty() {
                            name.clone()
                        } else {
                            let fields = record
                                .fields
                                .iter()
                                .map(|field| {
                                    let value = self.zero_expr_in(&field.ty, span, visiting)?;
                                    Ok(format!("{}: {value}", super::names::ident(&field.awl_name)))
                                })
                                .collect::<Result<Vec<_>, EmitError>>()?
                                .join(", ");
                            format!("{name}({fields})")
                        }
                    }
                    Some(NamedDef::Enum(variants)) => {
                        variants.first().cloned().ok_or_else(|| {
                            EmitError::new(span, format!("enum `{name}` has no variants"))
                        })?
                    }
                    Some(NamedDef::Alias(inner)) => {
                        self.zero_expr_in(&inner.clone(), span, visiting)?
                    }
                    None => {
                        return Err(EmitError::new(
                            span,
                            format!("reference to undeclared type `{name}`"),
                        ));
                    }
                };
                visiting.pop();
                Ok(rendered)
            }
        }
    }
}

/// Convert a declared `TypeRef` to a `GType`. `Dir` maps to Gleam `String`
/// (a content-addressed handle travels as its string form in the stopgap).
pub(super) fn type_ref_to_g(ty: &TypeRef) -> GType {
    match ty {
        TypeRef::Named { name, .. } => match name.as_str() {
            "Bool" => GType::Bool,
            "Int" => GType::Int,
            "Float" => GType::Float,
            "String" | "Dir" => GType::Str,
            "Nil" => GType::Nil,
            other => GType::Named(other.to_owned()),
        },
        TypeRef::List { inner, .. } => GType::List(Box::new(type_ref_to_g(inner))),
        TypeRef::Optional { inner, .. } => GType::Option(Box::new(type_ref_to_g(inner))),
    }
}

/// Build the type environment: declared types first (claiming their names
/// and enum variant constructors), then schema-door projections.
pub(super) fn build_env(document: &Document, root: Option<&Path>) -> Result<TypeEnv, EmitError> {
    let mut env = TypeEnv::default();
    for builtin in ["Bool", "Int", "Float", "String", "Nil", "True", "False"] {
        env.names.claim(builtin);
    }
    register_declared(document, &mut env)?;
    // Pass 2: schema doors project into the same registry.
    for decl in &document.types {
        let (value, span) = match &decl.body {
            TypeBody::SchemaInline { body, body_span } => {
                let value: Value = serde_json::from_str(body).map_err(|error| {
                    EmitError::new(
                        *body_span,
                        format!(
                            "the inline schema for `{}` is not valid JSON: {error}",
                            decl.name
                        ),
                    )
                })?;
                (value, *body_span)
            }
            TypeBody::SchemaImport { path, path_span } => {
                let Some(root) = root else {
                    return Err(EmitError::new(
                        *path_span,
                        format!(
                            "cannot resolve imported schema `{path}` without the document's \
                             directory (emit from the file's own directory)"
                        ),
                    ));
                };
                let text = std::fs::read_to_string(root.join(path)).map_err(|error| {
                    EmitError::new(
                        *path_span,
                        format!("cannot read imported schema `{path}`: {error}"),
                    )
                })?;
                let value: Value = serde_json::from_str(&text).map_err(|error| {
                    EmitError::new(
                        *path_span,
                        format!("imported schema `{path}` is not valid JSON: {error}"),
                    )
                })?;
                (value, *path_span)
            }
            TypeBody::Record { .. } | TypeBody::Enum { .. } => continue,
        };
        let defs = value
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut projector = super::project::Projector::new(&mut env, defs, span, decl.name.clone());
        let projected = projector.project(&value, &decl.name, 0)?;
        match projected {
            GType::Named(ref name) if name == &decl.name => {}
            other => {
                env.defs.insert(decl.name.clone(), NamedDef::Alias(other));
            }
        }
    }
    Ok(env)
}

/// Pass 1: declared shorthand records and enums register; door names are
/// claimed so synthesized names never steal them.
fn register_declared(document: &Document, env: &mut TypeEnv) -> Result<(), EmitError> {
    for decl in &document.types {
        if !env.names.claim(&decl.name) {
            return Err(EmitError::new(
                decl.name_span,
                format!("type `{}` is declared more than once", decl.name),
            ));
        }
        env.order.push(decl.name.clone());
        match &decl.body {
            TypeBody::Record { fields } => {
                let fields = fields
                    .iter()
                    .map(|field| FieldDef {
                        awl_name: field.name.clone(),
                        ty: type_ref_to_g(&field.ty),
                    })
                    .collect();
                env.defs
                    .insert(decl.name.clone(), NamedDef::Record(RecordDef { fields }));
            }
            TypeBody::Enum { variants } => {
                let mut names = Vec::new();
                for variant in variants {
                    if !env.names.claim(&variant.name) {
                        return Err(EmitError::new(
                            variant.span,
                            format!(
                                "enum variant `{}` collides with another generated Gleam \
                                 constructor",
                                variant.name
                            ),
                        ));
                    }
                    names.push(variant.name.clone());
                }
                env.defs.insert(decl.name.clone(), NamedDef::Enum(names));
            }
            TypeBody::SchemaInline { .. } | TypeBody::SchemaImport { .. } => {}
        }
    }
    Ok(())
}
