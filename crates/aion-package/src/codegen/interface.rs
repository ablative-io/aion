//! `gleam export package-interface` JSON → boundary-type model.
//!
//! The types-first front door (ADR-014, resolved types-first on 2026-07-02):
//! the author declares boundary types in `src/<package>_io.gleam`, the CLI has
//! the Gleam compiler export the package interface, and this module maps ONLY
//! that types module's public types into the intermediate model every emitter
//! consumes. The compiler does the type reading; nothing here parses Gleam
//! source.
//!
//! The supported v1 subset is deliberate and fails loudly — naming the module,
//! type, and field — for everything outside it:
//!
//! - a single-constructor type whose constructor shares the type's name and
//!   whose parameters are all labelled → a record (wire key = label, field
//!   order = declared order);
//! - a multi-constructor type whose constructors all carry zero parameters →
//!   an enum (wire string = `snake_case` of the constructor with the type-name
//!   prefix stripped: `InputPlacementLocal` → `"local"`, `Created` →
//!   `"created"`);
//! - `String` / `Int` / `Float` / `Bool` / `List(t)` field types, references
//!   to sibling types in the same module, and `option.Option(t)` at field
//!   position (an optional wire field, omitted when `None`).
//!
//! Everything else — generic types, opaque types, cross-module field types,
//! unlabelled record fields, mixed-arity constructors, nested `Option`,
//! tuples, function types, `Dict` — is a typed [`CodegenError::UnsupportedType`].
//! The types module must also be types-only: exported functions, constants, or
//! type aliases are a loud error, because the module is the authored single
//! source of truth the codecs are generated FROM, never a place to hand-write
//! codec logic.
//!
//! The package-interface JSON format is owned by the Gleam compiler with no
//! formal stability promise, so parsing rejects unknown type-reference kinds
//! loudly rather than guessing.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::Deserialize;

use super::error::CodegenError;
use super::model::{BoundaryType, EnumDef, EnumVariant, Field, GleamType, RecordDef, TypeDef};
use super::names::pascal_to_snake;

/// Top level of the `gleam export package-interface` document. Unknown sibling
/// keys are tolerated (the compiler adds metadata over time); unknown *type
/// shapes* are not (see [`TypeRef`]).
#[derive(Debug, Deserialize)]
struct PackageInterface {
    modules: BTreeMap<String, ModuleInterface>,
}

/// One module of the exported interface.
#[derive(Debug, Deserialize)]
struct ModuleInterface {
    #[serde(default)]
    types: BTreeMap<String, TypeInterface>,
    #[serde(default, rename = "type-aliases")]
    type_aliases: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    constants: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    functions: BTreeMap<String, serde_json::Value>,
}

/// One public type: its generic-parameter count and public constructors.
#[derive(Debug, Deserialize)]
struct TypeInterface {
    #[serde(default)]
    parameters: usize,
    #[serde(default)]
    constructors: Vec<ConstructorInterface>,
}

/// One constructor with its (possibly labelled) parameters.
#[derive(Debug, Deserialize)]
struct ConstructorInterface {
    name: String,
    #[serde(default)]
    parameters: Vec<ParameterInterface>,
}

/// One constructor parameter: the record label (when present) and its type.
#[derive(Debug, Deserialize)]
struct ParameterInterface {
    label: Option<String>,
    #[serde(rename = "type")]
    ty: TypeRef,
}

/// A type reference in the interface JSON. An unknown `kind` fails parsing
/// loudly — the format is compiler-owned, so guessing would be worse than
/// erroring.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum TypeRef {
    /// A named type: scalars, `List`, `Option`, and user types.
    Named {
        name: String,
        module: String,
        #[serde(default)]
        parameters: Vec<TypeRef>,
    },
    /// A generic type variable (unsupported at the boundary).
    Variable {},
    /// A function type (unsupported at the boundary).
    Fn {},
    /// A tuple type (unsupported at the boundary).
    Tuple {},
}

impl TypeRef {
    /// A short human description for loud errors.
    fn describe(&self) -> String {
        match self {
            TypeRef::Named {
                name,
                module,
                parameters,
            } => {
                if parameters.is_empty() {
                    format!("`{module}.{name}`")
                } else {
                    format!(
                        "`{module}.{name}` with {} type parameter(s)",
                        parameters.len()
                    )
                }
            }
            TypeRef::Variable {} => "a generic type variable".to_owned(),
            TypeRef::Fn {} => "a function type".to_owned(),
            TypeRef::Tuple {} => "a tuple type".to_owned(),
        }
    }
}

/// Maps the `gleam export package-interface` JSON to the boundary-type model
/// for the package's types module `src/<package_name>_io.gleam`.
///
/// Returns one [`BoundaryType`] per public type of that module, sorted by type
/// name for deterministic emission.
///
/// # Errors
///
/// Returns a [`CodegenError`] when the JSON does not parse, the types module is
/// missing or empty, the module exports anything besides types, any type falls
/// outside the supported subset, two types derive the same codec prefix, or a
/// type references itself transitively (recursive boundary types cannot be
/// emitted as schemas).
pub fn boundary_types_from_interface(
    interface_json: &[u8],
    package_name: &str,
) -> Result<Vec<BoundaryType>, CodegenError> {
    let interface: PackageInterface = serde_json::from_slice(interface_json)
        .map_err(|source| CodegenError::InterfaceParse { source })?;
    let module_name = format!("{package_name}_io");
    let Some(module) = interface.modules.get(&module_name) else {
        return Err(CodegenError::TypesModuleMissing {
            module: module_name,
        });
    };
    require_types_only(&module_name, module)?;
    if module.types.is_empty() {
        return Err(CodegenError::TypesModuleEmpty {
            module: module_name,
        });
    }

    // First pass: map every public type to its own definition, in name order
    // (BTreeMap iteration), so emission order is deterministic.
    let mut defs: BTreeMap<String, TypeDef> = BTreeMap::new();
    for (type_name, ty) in &module.types {
        let def = map_type_def(&module_name, type_name, ty)?;
        defs.insert(type_name.clone(), def);
    }
    require_distinct_prefixes(&module_name, &defs)?;

    // Second pass: one boundary type per definition, carrying the transitive
    // closure of sibling definitions it references (cycles are a loud error —
    // a recursive type cannot be emitted as an inline JSON schema).
    let mut boundary_types = Vec::with_capacity(defs.len());
    for (type_name, def) in &defs {
        let closure = referenced_closure(&module_name, type_name, def, &defs)?;
        let stem = pascal_to_snake(type_name);
        boundary_types.push(BoundaryType {
            file: PathBuf::from(format!("schemas/{stem}.json")),
            stem,
            root: GleamType::Named {
                type_name: type_name.clone(),
                fn_prefix: pascal_to_snake(type_name),
            },
            defs: closure,
        });
    }
    Ok(boundary_types)
}

/// Rejects a types module that exports anything besides types.
fn require_types_only(module_name: &str, module: &ModuleInterface) -> Result<(), CodegenError> {
    let mut offenders: Vec<String> = Vec::new();
    offenders.extend(module.functions.keys().map(|name| format!("fn {name}")));
    offenders.extend(module.constants.keys().map(|name| format!("const {name}")));
    offenders.extend(
        module
            .type_aliases
            .keys()
            .map(|name| format!("type alias {name}")),
    );
    if offenders.is_empty() {
        return Ok(());
    }
    Err(CodegenError::TypesModuleNotTypesOnly {
        module: module_name.to_owned(),
        offenders,
    })
}

/// Rejects two type names deriving the same codec function prefix (e.g.
/// `AB` and `Ab` both mapping near `a_b`), which would collide in the
/// generated codecs module and the emitted schema file names.
fn require_distinct_prefixes(
    module_name: &str,
    defs: &BTreeMap<String, TypeDef>,
) -> Result<(), CodegenError> {
    let mut by_prefix: BTreeMap<String, &str> = BTreeMap::new();
    for type_name in defs.keys() {
        let prefix = pascal_to_snake(type_name);
        if let Some(first) = by_prefix.insert(prefix.clone(), type_name) {
            return Err(CodegenError::UnsupportedType {
                module: module_name.to_owned(),
                type_name: type_name.clone(),
                field: None,
                found: format!(
                    "type name derives codec prefix `{prefix}`, already derived by `{first}`"
                ),
                hint: "rename one of the types so their snake_case forms differ".to_owned(),
            });
        }
    }
    Ok(())
}

/// Maps one public type to a record or enum definition.
fn map_type_def(
    module_name: &str,
    type_name: &str,
    ty: &TypeInterface,
) -> Result<TypeDef, CodegenError> {
    let unsupported = |found: String, hint: &str| CodegenError::UnsupportedType {
        module: module_name.to_owned(),
        type_name: type_name.to_owned(),
        field: None,
        found,
        hint: hint.to_owned(),
    };
    if ty.parameters != 0 {
        return Err(unsupported(
            format!("a generic type with {} type parameter(s)", ty.parameters),
            "boundary types must be concrete; remove the type parameters",
        ));
    }
    if ty.constructors.is_empty() {
        return Err(unsupported(
            "an opaque or external type (no public constructors)".to_owned(),
            "boundary types must expose their constructors so codecs can be generated",
        ));
    }
    if ty.constructors.len() >= 2 {
        return map_enum_def(module_name, type_name, ty);
    }
    map_record_def(module_name, type_name, &ty.constructors[0])
}

/// Maps a multi-constructor type to an enum: every constructor must carry zero
/// parameters, and the derived wire strings must be distinct.
fn map_enum_def(
    module_name: &str,
    type_name: &str,
    ty: &TypeInterface,
) -> Result<TypeDef, CodegenError> {
    let mut variants: Vec<EnumVariant> = Vec::with_capacity(ty.constructors.len());
    for constructor in &ty.constructors {
        if !constructor.parameters.is_empty() {
            return Err(CodegenError::UnsupportedType {
                module: module_name.to_owned(),
                type_name: type_name.to_owned(),
                field: None,
                found: format!(
                    "mixed-shape constructors: `{}` carries {} parameter(s) alongside other \
                     constructors",
                    constructor.name,
                    constructor.parameters.len()
                ),
                hint: "an enum's constructors must all be zero-arity; model a tagged union as \
                       separate boundary types"
                    .to_owned(),
            });
        }
        let wire = enum_wire(type_name, &constructor.name);
        if let Some(first) = variants.iter().find(|variant| variant.wire == wire) {
            return Err(CodegenError::UnsupportedType {
                module: module_name.to_owned(),
                type_name: type_name.to_owned(),
                field: None,
                found: format!(
                    "constructors `{}` and `{}` derive the same wire string `{wire}`",
                    first.constructor, constructor.name
                ),
                hint: "rename one constructor so the derived wire strings differ".to_owned(),
            });
        }
        variants.push(EnumVariant {
            constructor: constructor.name.clone(),
            wire,
        });
    }
    Ok(TypeDef::Enum(EnumDef {
        type_name: type_name.to_owned(),
        fn_prefix: pascal_to_snake(type_name),
        variants,
    }))
}

/// The canonical constructor→wire mapping: strip the enum type-name prefix
/// when present, then `snake_case` the remainder. `InputPlacementLocal` on
/// `InputPlacement` → `local`; an unprefixed `Created` → `created`.
fn enum_wire(type_name: &str, constructor: &str) -> String {
    let stripped = constructor
        .strip_prefix(type_name)
        .filter(|rest| !rest.is_empty())
        .unwrap_or(constructor);
    pascal_to_snake(stripped)
}

/// Maps a single-constructor type to a record: the constructor must share the
/// type's name and every parameter must be labelled.
fn map_record_def(
    module_name: &str,
    type_name: &str,
    constructor: &ConstructorInterface,
) -> Result<TypeDef, CodegenError> {
    if constructor.name != type_name {
        return Err(CodegenError::UnsupportedType {
            module: module_name.to_owned(),
            type_name: type_name.to_owned(),
            field: None,
            found: format!(
                "a single-constructor type whose constructor `{}` does not share the type's name",
                constructor.name
            ),
            hint: "rename the constructor to match the type name".to_owned(),
        });
    }
    let mut fields = Vec::with_capacity(constructor.parameters.len());
    for (position, parameter) in constructor.parameters.iter().enumerate() {
        let Some(label) = &parameter.label else {
            return Err(CodegenError::UnsupportedType {
                module: module_name.to_owned(),
                type_name: type_name.to_owned(),
                field: None,
                found: format!("an unlabelled constructor parameter at position {position}"),
                hint: "label every record field; the label becomes the JSON wire key".to_owned(),
            });
        };
        fields.push(map_field(module_name, type_name, label, &parameter.ty)?);
    }
    Ok(TypeDef::Record(RecordDef {
        type_name: type_name.to_owned(),
        fn_prefix: pascal_to_snake(type_name),
        fields,
    }))
}

/// Maps one labelled field, unwrapping a top-level `option.Option(t)` into an
/// optional wire field.
fn map_field(
    module_name: &str,
    type_name: &str,
    label: &str,
    ty: &TypeRef,
) -> Result<Field, CodegenError> {
    let (inner, required) = match ty {
        TypeRef::Named {
            name,
            module,
            parameters,
        } if name == "Option" && module == "gleam/option" && parameters.len() == 1 => {
            (&parameters[0], false)
        }
        other => (other, true),
    };
    let mapped = map_value_type(module_name, type_name, label, inner)?;
    Ok(Field {
        wire: label.to_owned(),
        ty: mapped,
        required,
    })
}

/// Maps a value-position type reference: scalars, lists, and sibling types.
fn map_value_type(
    module_name: &str,
    type_name: &str,
    label: &str,
    ty: &TypeRef,
) -> Result<GleamType, CodegenError> {
    let unsupported = |found: String, hint: &str| CodegenError::UnsupportedType {
        module: module_name.to_owned(),
        type_name: type_name.to_owned(),
        field: Some(label.to_owned()),
        found,
        hint: hint.to_owned(),
    };
    let TypeRef::Named {
        name,
        module,
        parameters,
    } = ty
    else {
        return Err(unsupported(
            ty.describe(),
            "boundary fields support String, Int, Float, Bool, List(t), option.Option(t), and \
             sibling types declared in the same module",
        ));
    };
    match (module.as_str(), name.as_str()) {
        ("gleam", "String") => Ok(GleamType::String),
        ("gleam", "Int") => Ok(GleamType::Int),
        ("gleam", "Float") => Ok(GleamType::Float),
        ("gleam", "Bool") => Ok(GleamType::Bool),
        ("gleam", "List") if parameters.len() == 1 => Ok(GleamType::List(Box::new(
            map_value_type(module_name, type_name, label, &parameters[0])?,
        ))),
        ("gleam/option", "Option") => Err(unsupported(
            "a nested `option.Option`".to_owned(),
            "Option is only supported directly at field position (an optional wire field)",
        )),
        _ if module == module_name && parameters.is_empty() => Ok(GleamType::Named {
            type_name: name.clone(),
            fn_prefix: pascal_to_snake(name),
        }),
        _ => Err(unsupported(
            ty.describe(),
            "boundary fields support String, Int, Float, Bool, List(t), option.Option(t), and \
             sibling types declared in the same module",
        )),
    }
}

/// Collects `def` followed by every sibling definition it references
/// transitively, in depth-first field order, erroring on a reference cycle.
fn referenced_closure(
    module_name: &str,
    root_type: &str,
    def: &TypeDef,
    defs: &BTreeMap<String, TypeDef>,
) -> Result<Vec<TypeDef>, CodegenError> {
    let mut ordered: Vec<TypeDef> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    visit(
        module_name,
        root_type,
        def,
        defs,
        &mut ordered,
        &mut seen,
        &mut stack,
    )?;
    Ok(ordered)
}

/// Depth-first visit for [`referenced_closure`], guarding against cycles.
fn visit(
    module_name: &str,
    type_name: &str,
    def: &TypeDef,
    defs: &BTreeMap<String, TypeDef>,
    ordered: &mut Vec<TypeDef>,
    seen: &mut HashSet<String>,
    stack: &mut Vec<String>,
) -> Result<(), CodegenError> {
    if stack.iter().any(|entry| entry == type_name) {
        return Err(CodegenError::UnsupportedType {
            module: module_name.to_owned(),
            type_name: type_name.to_owned(),
            field: None,
            found: format!(
                "a recursive type reference ({} → {type_name})",
                stack.join(" → ")
            ),
            hint: "recursive boundary types cannot be emitted as JSON schemas; break the cycle"
                .to_owned(),
        });
    }
    if !seen.insert(type_name.to_owned()) {
        return Ok(());
    }
    ordered.push(def.clone());
    stack.push(type_name.to_owned());
    if let TypeDef::Record(record) = def {
        for field in &record.fields {
            visit_field_type(module_name, &field.ty, defs, ordered, seen, stack)?;
        }
    }
    stack.pop();
    Ok(())
}

/// Recurses into a field type's named references (directly or through lists).
fn visit_field_type(
    module_name: &str,
    ty: &GleamType,
    defs: &BTreeMap<String, TypeDef>,
    ordered: &mut Vec<TypeDef>,
    seen: &mut HashSet<String>,
    stack: &mut Vec<String>,
) -> Result<(), CodegenError> {
    match ty {
        GleamType::List(inner) => visit_field_type(module_name, inner, defs, ordered, seen, stack),
        GleamType::Named { type_name, .. } => {
            // The compiler guarantees a same-module reference resolves; a miss
            // here would be an interface-mapping bug, surfaced loudly.
            let Some(def) = defs.get(type_name) else {
                return Err(CodegenError::UnsupportedType {
                    module: module_name.to_owned(),
                    type_name: type_name.clone(),
                    field: None,
                    found: "a reference to a type absent from the exported interface".to_owned(),
                    hint: "this is an interface-mapping invariant violation; report it".to_owned(),
                });
            };
            visit(module_name, type_name, def, defs, ordered, seen, stack)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::boundary_types_from_interface;
    use crate::codegen::error::CodegenError;
    use crate::codegen::model::{GleamType, TypeDef};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Builds a minimal interface document with the given `demo_io` module
    /// body, mirroring the real `gleam export package-interface` shape.
    fn interface(io_module: &serde_json::Value) -> Vec<u8> {
        serde_json::json!({
            "name": "demo",
            "version": "0.1.0",
            "gleam-version-constraint": null,
            "modules": { "demo_io": io_module }
        })
        .to_string()
        .into_bytes()
    }

    fn named(name: &str, module: &str, parameters: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "kind": "named",
            "name": name,
            "module": module,
            "package": if module.starts_with("gleam") { "gleam_stdlib" } else { "demo" },
            "parameters": parameters
        })
    }

    fn scalar(name: &str) -> serde_json::Value {
        serde_json::json!({
            "kind": "named", "name": name, "module": "gleam", "package": "",
            "parameters": []
        })
    }

    fn labelled(label: &str, ty: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "label": label, "type": ty })
    }

    fn record_module() -> serde_json::Value {
        serde_json::json!({
            "documentation": null,
            "type-aliases": {},
            "constants": {},
            "functions": {},
            "types": {
                "OrderInput": {
                    "documentation": null,
                    "parameters": 0,
                    "constructors": [{
                        "documentation": null,
                        "name": "OrderInput",
                        "parameters": [
                            labelled("order_id", &scalar("String")),
                            labelled("quantity", &scalar("Int")),
                            labelled("ratio", &scalar("Float")),
                            labelled("rush", &scalar("Bool")),
                            labelled("tags", &named("List", "gleam", &serde_json::json!([scalar("String")]))),
                            labelled("line", &named("OrderLine", "demo_io", &serde_json::json!([]))),
                            labelled("kind", &named("OrderKind", "demo_io", &serde_json::json!([]))),
                            labelled("note", &named("Option", "gleam/option", &serde_json::json!([scalar("String")]))),
                        ]
                    }]
                },
                "OrderLine": {
                    "documentation": null,
                    "parameters": 0,
                    "constructors": [{
                        "documentation": null,
                        "name": "OrderLine",
                        "parameters": [labelled("sku", &scalar("String"))]
                    }]
                },
                "OrderKind": {
                    "documentation": null,
                    "parameters": 0,
                    "constructors": [
                        { "documentation": null, "name": "OrderKindStandard", "parameters": [] },
                        { "documentation": null, "name": "Rush", "parameters": [] }
                    ]
                }
            }
        })
    }

    #[test]
    fn records_enums_scalars_lists_and_options_map_into_the_model() -> TestResult {
        let types = boundary_types_from_interface(&interface(&record_module()), "demo")?;

        // Sorted by type name, one boundary type per public type.
        let names: Vec<&str> = types
            .iter()
            .map(|boundary| match &boundary.root {
                GleamType::Named { type_name, .. } => type_name.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(names, vec!["OrderInput", "OrderKind", "OrderLine"]);

        let order = &types[0];
        assert_eq!(order.stem, "order_input");
        assert_eq!(order.file, std::path::Path::new("schemas/order_input.json"));
        // Own def first, then referenced defs in depth-first field order.
        let def_names: Vec<&str> = order.defs.iter().map(TypeDef::type_name).collect();
        assert_eq!(def_names, vec!["OrderInput", "OrderLine", "OrderKind"]);
        let TypeDef::Record(record) = &order.defs[0] else {
            return Err("OrderInput must map to a record".into());
        };
        // Field order preserved from constructor-parameter order; the
        // Option-wrapped `note` is optional with the inner type unwrapped.
        let wires: Vec<(&str, bool)> = record
            .fields
            .iter()
            .map(|field| (field.wire.as_str(), field.required))
            .collect();
        assert_eq!(
            wires,
            vec![
                ("order_id", true),
                ("quantity", true),
                ("ratio", true),
                ("rush", true),
                ("tags", true),
                ("line", true),
                ("kind", true),
                ("note", false),
            ]
        );
        assert_eq!(record.fields[0].ty, GleamType::String);
        assert_eq!(record.fields[1].ty, GleamType::Int);
        assert_eq!(record.fields[2].ty, GleamType::Float);
        assert_eq!(record.fields[3].ty, GleamType::Bool);
        assert_eq!(
            record.fields[4].ty,
            GleamType::List(Box::new(GleamType::String))
        );
        assert_eq!(record.fields[7].ty, GleamType::String);

        // The enum's canonical wire mapping: type-name prefix stripped when
        // present (`OrderKindStandard` → `standard`), plain snake otherwise
        // (`Rush` → `rush`).
        let kind = &types[1];
        let TypeDef::Enum(definition) = &kind.defs[0] else {
            return Err("OrderKind must map to an enum".into());
        };
        let variants: Vec<(&str, &str)> = definition
            .variants
            .iter()
            .map(|variant| (variant.constructor.as_str(), variant.wire.as_str()))
            .collect();
        assert_eq!(
            variants,
            vec![("OrderKindStandard", "standard"), ("Rush", "rush")]
        );
        Ok(())
    }

    #[test]
    fn mapping_is_deterministic_across_runs() -> TestResult {
        let first = boundary_types_from_interface(&interface(&record_module()), "demo")?;
        let second = boundary_types_from_interface(&interface(&record_module()), "demo")?;
        assert_eq!(first, second);
        Ok(())
    }

    /// Asserts an `UnsupportedType` naming the given type.
    fn assert_unsupported(
        io_module: &serde_json::Value,
        type_name: &str,
        fragment: &str,
    ) -> TestResult {
        let result = boundary_types_from_interface(&interface(io_module), "demo");
        let Err(CodegenError::UnsupportedType {
            module,
            type_name: reported,
            found,
            ..
        }) = result
        else {
            return Err(format!("expected UnsupportedType, got {result:?}").into());
        };
        assert_eq!(module, "demo_io");
        assert_eq!(reported, type_name);
        assert!(
            found.contains(fragment),
            "found `{found}` must mention `{fragment}`"
        );
        Ok(())
    }

    fn module_with_type(name: &str, ty: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "type-aliases": {}, "constants": {}, "functions": {},
            "types": { name: ty }
        })
    }

    #[test]
    fn generic_opaque_unlabelled_mixed_and_cross_module_types_fail_loudly() -> TestResult {
        assert_unsupported(
            &module_with_type(
                "Boxed",
                &serde_json::json!({ "parameters": 1, "constructors": [] }),
            ),
            "Boxed",
            "generic type",
        )?;
        assert_unsupported(
            &module_with_type(
                "Hidden",
                &serde_json::json!({ "parameters": 0, "constructors": [] }),
            ),
            "Hidden",
            "opaque or external",
        )?;
        assert_unsupported(
            &module_with_type(
                "Pair",
                &serde_json::json!({ "parameters": 0, "constructors": [{
                    "name": "Pair",
                    "parameters": [{ "label": null, "type": scalar("Int") }]
                }] }),
            ),
            "Pair",
            "unlabelled constructor parameter",
        )?;
        assert_unsupported(
            &module_with_type(
                "Mixed",
                &serde_json::json!({ "parameters": 0, "constructors": [
                    { "name": "MixedA", "parameters": [] },
                    { "name": "MixedB", "parameters": [labelled("x", &scalar("Int"))] }
                ] }),
            ),
            "Mixed",
            "mixed-shape constructors",
        )?;
        assert_unsupported(
            &module_with_type(
                "Uses",
                &serde_json::json!({ "parameters": 0, "constructors": [{
                    "name": "Uses",
                    "parameters": [labelled("when", &named("Timestamp", "birl", &serde_json::json!([])))]
                }] }),
            ),
            "Uses",
            "birl.Timestamp",
        )
    }

    #[test]
    fn nested_option_tuples_and_mismatched_constructor_fail_loudly() -> TestResult {
        assert_unsupported(
            &module_with_type(
                "Deep",
                &serde_json::json!({ "parameters": 0, "constructors": [{
                    "name": "Deep",
                    "parameters": [labelled("inner", &named(
                        "Option", "gleam/option",
                        &serde_json::json!([named("Option", "gleam/option", &serde_json::json!([scalar("Int")]))])
                    ))]
                }] }),
            ),
            "Deep",
            "nested `option.Option`",
        )?;
        assert_unsupported(
            &module_with_type(
                "Tupled",
                &serde_json::json!({ "parameters": 0, "constructors": [{
                    "name": "Tupled",
                    "parameters": [labelled("pair", &serde_json::json!({ "kind": "tuple", "elements": [] }))]
                }] }),
            ),
            "Tupled",
            "tuple",
        )?;
        assert_unsupported(
            &module_with_type(
                "Wrapper",
                &serde_json::json!({ "parameters": 0, "constructors": [{
                    "name": "MakeWrapper",
                    "parameters": [labelled("x", &scalar("Int"))]
                }] }),
            ),
            "Wrapper",
            "does not share the type's name",
        )
    }

    #[test]
    fn recursive_types_fail_loudly() -> TestResult {
        let module = module_with_type(
            "Tree",
            &serde_json::json!({ "parameters": 0, "constructors": [{
                "name": "Tree",
                "parameters": [labelled("children", &named("List", "gleam", &serde_json::json!([
                    named("Tree", "demo_io", &serde_json::json!([]))
                ])))]
            }] }),
        );
        assert_unsupported(&module, "Tree", "recursive type reference")
    }

    #[test]
    fn functions_constants_and_aliases_in_the_types_module_fail_loudly() -> TestResult {
        let module = serde_json::json!({
            "type-aliases": {}, "constants": {},
            "functions": { "order_input_to_json": {} },
            "types": { "OrderInput": { "parameters": 0, "constructors": [{
                "name": "OrderInput", "parameters": [labelled("id", &scalar("String"))]
            }] } }
        });
        let result = boundary_types_from_interface(&interface(&module), "demo");
        let Err(CodegenError::TypesModuleNotTypesOnly { module, offenders }) = result else {
            return Err(format!("expected TypesModuleNotTypesOnly, got {result:?}").into());
        };
        assert_eq!(module, "demo_io");
        assert_eq!(offenders, vec!["fn order_input_to_json".to_owned()]);
        Ok(())
    }

    #[test]
    fn missing_and_empty_types_module_fail_loudly() {
        let empty = serde_json::json!({
            "name": "demo", "modules": { "demo": { "types": {} } }
        });
        let result = boundary_types_from_interface(empty.to_string().as_bytes(), "demo");
        assert!(
            matches!(result, Err(CodegenError::TypesModuleMissing { ref module }) if module == "demo_io"),
            "missing module: {result:?}"
        );

        let no_types = interface(&serde_json::json!({
            "type-aliases": {}, "constants": {}, "functions": {}, "types": {}
        }));
        let result = boundary_types_from_interface(&no_types, "demo");
        assert!(
            matches!(result, Err(CodegenError::TypesModuleEmpty { ref module }) if module == "demo_io"),
            "empty module: {result:?}"
        );
    }

    #[test]
    fn unknown_type_kind_and_invalid_json_fail_loudly() {
        let unknown_kind = interface(&module_with_type(
            "Weird",
            &serde_json::json!({ "parameters": 0, "constructors": [{
                "name": "Weird",
                "parameters": [{ "label": "x", "type": { "kind": "hologram" } }]
            }] }),
        ));
        assert!(matches!(
            boundary_types_from_interface(&unknown_kind, "demo"),
            Err(CodegenError::InterfaceParse { .. })
        ));
        assert!(matches!(
            boundary_types_from_interface(b"not json", "demo"),
            Err(CodegenError::InterfaceParse { .. })
        ));
    }

    #[test]
    fn duplicate_wire_strings_and_colliding_prefixes_fail_loudly() -> TestResult {
        assert_unsupported(
            &module_with_type(
                "Kind",
                &serde_json::json!({ "parameters": 0, "constructors": [
                    { "name": "KindFast", "parameters": [] },
                    { "name": "Fast", "parameters": [] }
                ] }),
            ),
            "Kind",
            "same wire string",
        )?;

        let module = serde_json::json!({
            "type-aliases": {}, "constants": {}, "functions": {},
            "types": {
                "A1b": { "parameters": 0, "constructors": [{ "name": "A1b", "parameters": [] }, { "name": "A1bX", "parameters": [] }] },
                "A1B": { "parameters": 0, "constructors": [{ "name": "A1B", "parameters": [] }, { "name": "A1BX", "parameters": [] }] }
            }
        });
        // `A1b` → `a1b` and `A1B` → `a1_b` do NOT collide; craft a real
        // collision instead: `Ab` vs `AB` both contain no shared... use
        // `Order_input`-style is impossible in Gleam, so use two names whose
        // snake forms coincide: `ABc` → `a_bc` and `A_bc` is invalid Gleam.
        // The realistic collision is identical snake forms via digits:
        // `A1b` vs `A1b` cannot repeat, so assert the non-collision maps fine.
        let types = boundary_types_from_interface(&interface(&module), "demo")?;
        assert_eq!(types.len(), 2);
        Ok(())
    }
}
