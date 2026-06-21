//! Emission of the generated wire-compat golden (remote tiers only).
//!
//! For each value type a remote activity carries, this derives a gleeunit test
//! pinning the type's encoded wire shape:
//!
//! ```gleam
//! io.<prefix>_to_json(<canonical sample>) |> json.to_string |> should.equal("<literal>")
//! ```
//!
//! Both the canonical Gleam sample value and the expected compact-JSON literal
//! are *derived* from the same schema the codecs come from (checklist C3) — no
//! hand-written literal — by walking the record/enum definitions: required
//! scalars take their zero value (`""`, `0`, `0.0`, `false`), lists take the
//! empty list, nested records and enums recurse, and an optional field takes
//! `option.None`, which the encoder omits from the wire, so it never appears in
//! the literal. The literal is exactly what `json.to_string` produces for that
//! value (compact, keys in field order), so the test is a true fixed point: it
//! passes the moment the codecs and the literal agree and fails the instant a
//! wire shape moves.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use super::activity_model::{ResolvedActivity, ResolvedType};
use super::activity_wrappers::GENERATED_ACTIVITY_HEADER;
use super::error::CodegenError;
use super::schema::{EnumDef, GleamType, RecordDef, TypeDef};

/// Emits `test/<pkg>_wire_compat_test.gleam`: one gleeunit test per distinct
/// value type the remote `activities` carry (input before output, declaration
/// order, deduplicated by type name), each pinning the type's encoded wire
/// shape against a literal derived from its schema.
///
/// # Errors
///
/// Returns [`CodegenError::GoldenTypeUnresolved`] if a value type or a nested
/// field references a named type absent from its schema artifact — a generator
/// invariant violation, never bad author input, since the schema walker emits
/// every nested record and enum into the artifact.
pub(crate) fn emit(
    package_name: &str,
    activities: &[&ResolvedActivity],
) -> Result<String, CodegenError> {
    let mut tests: Vec<(String, String, String)> = Vec::new();
    let mut used_option = false;
    for value_type in distinct_value_types(activities) {
        let (value, literal) = value_type_sample(value_type, &mut used_option)?;
        tests.push((value_type.fn_prefix.clone(), value, literal));
    }

    let mut out = String::new();
    let _ = writeln!(out, "{GENERATED_ACTIVITY_HEADER}");
    out.push_str("////\n");
    let _ = writeln!(
        out,
        "//// Wire-compat goldens for the `{package_name}` remote activities: each test pins"
    );
    out.push_str(
        "//// a value type's encoded wire shape against a literal derived from its schema.\n",
    );
    out.push('\n');
    let _ = writeln!(out, "import {package_name}_io as io");
    out.push_str("import gleam/json\n");
    if used_option {
        out.push_str("import gleam/option\n");
    }
    out.push_str("import gleeunit/should\n");

    for (prefix, value, literal) in &tests {
        let _ = write!(
            out,
            "\npub fn {prefix}_wire_test() {{\n  io.{prefix}_to_json({value})\n  \
             |> json.to_string\n  |> should.equal(\"{}\")\n}}\n",
            gleam_escape(literal)
        );
    }
    Ok(out)
}

/// Collects the value types the remote activities carry, in deterministic order
/// — input before output, declaration order, first occurrence wins — so a
/// type shared across activities is pinned exactly once.
fn distinct_value_types<'a>(activities: &[&'a ResolvedActivity<'a>]) -> Vec<&'a ResolvedType<'a>> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut types: Vec<&ResolvedType> = Vec::new();
    for activity in activities {
        for value_type in [&activity.input, &activity.output] {
            if seen.insert(value_type.gleam_type.as_str()) {
                types.push(value_type);
            }
        }
    }
    types
}

/// Derives the canonical sample value and its wire literal for one value type.
fn value_type_sample(
    value_type: &ResolvedType,
    used_option: &mut bool,
) -> Result<(String, String), CodegenError> {
    named_sample(
        &value_type.gleam_type,
        &value_type.artifact.defs,
        &value_type.gleam_type,
        value_type.artifact.file.as_path(),
        used_option,
    )
}

/// Resolves a named type to its definition and derives its sample.
fn named_sample(
    type_name: &str,
    defs: &[TypeDef],
    root_type: &str,
    file: &Path,
    used_option: &mut bool,
) -> Result<(String, String), CodegenError> {
    let def = defs
        .iter()
        .find(|def| type_def_name(def) == type_name)
        .ok_or_else(|| CodegenError::GoldenTypeUnresolved {
            root_type: root_type.to_owned(),
            missing: type_name.to_owned(),
            file: file.to_path_buf(),
        })?;
    match def {
        TypeDef::Record(record) => record_sample(record, defs, root_type, file, used_option),
        TypeDef::Enum(definition) => enum_sample(definition, root_type, file),
    }
}

/// Derives a record's sample: every required field, in order; optional fields
/// take `option.None` and are omitted from the literal, matching the encoder.
fn record_sample(
    record: &RecordDef,
    defs: &[TypeDef],
    root_type: &str,
    file: &Path,
    used_option: &mut bool,
) -> Result<(String, String), CodegenError> {
    if record.fields.is_empty() {
        return Ok((format!("io.{}", record.type_name), "{}".to_owned()));
    }
    let mut value_fields: Vec<String> = Vec::with_capacity(record.fields.len());
    let mut literal_fields: Vec<String> = Vec::new();
    for field in &record.fields {
        if field.required {
            let (value, literal) = type_sample(&field.ty, defs, root_type, file, used_option)?;
            value_fields.push(format!("{}: {value}", field.wire));
            literal_fields.push(format!("\"{}\":{literal}", field.wire));
        } else {
            *used_option = true;
            value_fields.push(format!("{}: option.None", field.wire));
        }
    }
    Ok((
        format!("io.{}({})", record.type_name, value_fields.join(", ")),
        format!("{{{}}}", literal_fields.join(",")),
    ))
}

/// Derives an enum's sample: the first variant's constructor and wire string.
fn enum_sample(
    definition: &EnumDef,
    root_type: &str,
    file: &Path,
) -> Result<(String, String), CodegenError> {
    let first = definition
        .variants
        .first()
        .ok_or_else(|| CodegenError::GoldenTypeUnresolved {
            root_type: root_type.to_owned(),
            missing: format!("{} (enum has no variants)", definition.type_name),
            file: file.to_path_buf(),
        })?;
    Ok((
        format!("io.{}", first.constructor),
        format!("\"{}\"", first.wire),
    ))
}

/// Derives the sample value and literal for a field type.
fn type_sample(
    ty: &GleamType,
    defs: &[TypeDef],
    root_type: &str,
    file: &Path,
    used_option: &mut bool,
) -> Result<(String, String), CodegenError> {
    Ok(match ty {
        GleamType::String => ("\"\"".to_owned(), "\"\"".to_owned()),
        GleamType::Int => ("0".to_owned(), "0".to_owned()),
        GleamType::Float => ("0.0".to_owned(), "0.0".to_owned()),
        GleamType::Bool => ("False".to_owned(), "false".to_owned()),
        GleamType::List(_) => ("[]".to_owned(), "[]".to_owned()),
        GleamType::Named { type_name, .. } => {
            return named_sample(type_name, defs, root_type, file, used_option);
        }
    })
}

/// The generated type name of a definition.
fn type_def_name(def: &TypeDef) -> &str {
    match def {
        TypeDef::Record(record) => &record.type_name,
        TypeDef::Enum(definition) => &definition.type_name,
    }
}

/// Escapes a JSON literal for embedding inside a Gleam double-quoted string.
fn gleam_escape(literal: &str) -> String {
    literal.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::emit;
    use crate::codegen::activity_model::{ResolvedActivity, ResolvedType};
    use crate::codegen::declaration::{ActivityDeclaration, Tier};
    use crate::codegen::error::CodegenError;
    use crate::codegen::schema::{
        EnumDef, EnumVariant, Field, GleamType, RecordDef, SchemaArtifact, TypeDef,
    };

    /// `OrderInputLine` -> `order_input_line`, matching the schema walker's
    /// derived function prefixes.
    fn snake(pascal: &str) -> String {
        let mut out = String::with_capacity(pascal.len() + 4);
        for (index, ch) in pascal.char_indices() {
            if ch.is_ascii_uppercase() {
                if index != 0 {
                    out.push('_');
                }
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn named(type_name: &str) -> GleamType {
        GleamType::Named {
            type_name: type_name.to_owned(),
            fn_prefix: snake(type_name),
        }
    }

    fn field(wire: &str, ty: GleamType, required: bool) -> Field {
        Field {
            wire: wire.to_owned(),
            ty,
            required,
        }
    }

    fn record_def(type_name: &str, fields: Vec<Field>) -> TypeDef {
        TypeDef::Record(RecordDef {
            type_name: type_name.to_owned(),
            fn_prefix: snake(type_name),
            pointer: String::new(),
            fields,
        })
    }

    fn artifact(type_name: &str, defs: Vec<TypeDef>) -> SchemaArtifact {
        SchemaArtifact {
            file: PathBuf::from(format!("schemas/{}.json", snake(type_name))),
            stem: snake(type_name),
            root: named(type_name),
            defs,
        }
    }

    fn declaration(name: &str, input: &str, output: &str) -> ActivityDeclaration {
        ActivityDeclaration {
            name: name.to_owned(),
            tier: Tier::RemotePython,
            input_type: input.to_owned(),
            output_type: output.to_owned(),
        }
    }

    fn resolved<'a>(
        declaration: &'a ActivityDeclaration,
        input: &'a SchemaArtifact,
        output: &'a SchemaArtifact,
    ) -> ResolvedActivity<'a> {
        ResolvedActivity {
            declaration,
            input: ResolvedType {
                gleam_type: declaration.input_type.clone(),
                fn_prefix: snake(&declaration.input_type),
                artifact: input,
            },
            output: ResolvedType {
                gleam_type: declaration.output_type.clone(),
                fn_prefix: snake(&declaration.output_type),
                artifact: output,
            },
        }
    }

    #[test]
    fn all_required_scalars_pin_to_a_zero_value_literal() -> Result<(), Box<dyn std::error::Error>>
    {
        let order = artifact(
            "OrderInput",
            vec![record_def(
                "OrderInput",
                vec![
                    field("order_id", GleamType::String, true),
                    field("quantity", GleamType::Int, true),
                    field("amount", GleamType::Float, true),
                    field("rush", GleamType::Bool, true),
                ],
            )],
        );
        let receipt = artifact(
            "Receipt",
            vec![record_def(
                "Receipt",
                vec![field("id", GleamType::String, true)],
            )],
        );
        let charge = declaration("charge", "OrderInput", "Receipt");
        let activities = [resolved(&charge, &order, &receipt)];
        let refs: Vec<&ResolvedActivity> = activities.iter().collect();

        let module = emit("order_saga", &refs)?;

        assert!(module.starts_with(super::GENERATED_ACTIVITY_HEADER));
        assert!(module.contains("import order_saga_io as io\n"));
        assert!(module.contains("import gleam/json\n"));
        assert!(module.contains("import gleeunit/should\n"));
        // No optional field anywhere → no gleam/option import.
        assert!(!module.contains("import gleam/option"));
        assert!(module.contains(
            "pub fn order_input_wire_test() {\n  \
             io.order_input_to_json(io.OrderInput(order_id: \"\", quantity: 0, amount: 0.0, rush: False))\n  \
             |> json.to_string\n  \
             |> should.equal(\"{\\\"order_id\\\":\\\"\\\",\\\"quantity\\\":0,\\\"amount\\\":0.0,\\\"rush\\\":false}\")\n}\n"
        ));
        // Output type pinned too.
        assert!(module.contains("pub fn receipt_wire_test() {"));
        Ok(())
    }

    #[test]
    fn optionals_lists_nested_records_and_enums_are_derived()
    -> Result<(), Box<dyn std::error::Error>> {
        // OrderInput { order_id: String (req), tags: List(String) (req),
        //   line: OrderInputLine (req, nested record),
        //   kind: OrderInputKind (req, enum), note: String (optional) }
        let order = artifact(
            "OrderInput",
            vec![
                record_def(
                    "OrderInput",
                    vec![
                        field("order_id", GleamType::String, true),
                        field("tags", GleamType::List(Box::new(GleamType::String)), true),
                        field("line", named("OrderInputLine"), true),
                        field("kind", named("OrderInputKind"), true),
                        field("note", GleamType::String, false),
                    ],
                ),
                record_def(
                    "OrderInputLine",
                    vec![field("sku", GleamType::String, true)],
                ),
                TypeDef::Enum(EnumDef {
                    type_name: "OrderInputKind".to_owned(),
                    fn_prefix: "order_input_kind".to_owned(),
                    pointer: "/properties/kind".to_owned(),
                    variants: vec![
                        EnumVariant {
                            constructor: "OrderInputKindStandard".to_owned(),
                            wire: "standard".to_owned(),
                        },
                        EnumVariant {
                            constructor: "OrderInputKindRush".to_owned(),
                            wire: "rush".to_owned(),
                        },
                    ],
                }),
            ],
        );
        let ok = artifact(
            "Ok",
            vec![record_def("Ok", vec![field("done", GleamType::Bool, true)])],
        );
        let place = declaration("place", "OrderInput", "Ok");
        let activities = [resolved(&place, &order, &ok)];
        let refs: Vec<&ResolvedActivity> = activities.iter().collect();

        let module = emit("demo", &refs)?;

        // Optional field present somewhere → gleam/option imported.
        assert!(module.contains("import gleam/option\n"));
        // Nested record recurses to io.OrderInputLine(...); enum takes the
        // first variant; list is empty; optional `note` is None and omitted
        // from the literal.
        assert!(module.contains(
            "io.order_input_to_json(io.OrderInput(order_id: \"\", tags: [], \
             line: io.OrderInputLine(sku: \"\"), kind: io.OrderInputKindStandard, \
             note: option.None))"
        ));
        assert!(module.contains(
            "|> should.equal(\"{\\\"order_id\\\":\\\"\\\",\\\"tags\\\":[],\\\"line\\\":{\\\"sku\\\":\\\"\\\"},\\\"kind\\\":\\\"standard\\\"}\")"
        ));
        Ok(())
    }

    #[test]
    fn unresolved_named_type_is_an_internal_error() -> Result<(), Box<dyn std::error::Error>> {
        // A field references a named type with no matching def — a generator
        // invariant violation that must surface, not panic.
        let broken = artifact(
            "OrderInput",
            vec![record_def(
                "OrderInput",
                vec![field("line", named("MissingLine"), true)],
            )],
        );
        let out = artifact("Ok", vec![record_def("Ok", vec![])]);
        let place = declaration("place", "OrderInput", "Ok");
        let activities = [resolved(&place, &broken, &out)];
        let refs: Vec<&ResolvedActivity> = activities.iter().collect();

        let result = emit("demo", &refs);
        let Err(CodegenError::GoldenTypeUnresolved { missing, .. }) = result else {
            return Err(format!("expected GoldenTypeUnresolved, got {result:?}").into());
        };
        assert_eq!(missing, "MissingLine");
        Ok(())
    }
}
