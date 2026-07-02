//! The intermediate boundary-type model every codegen emitter consumes.
//!
//! Types-first (ADR-014, resolved 2026-07-02): the authored source of truth is
//! the project's Gleam types module `src/<package>_io.gleam`. The interface
//! front-end ([`super::interface`]) maps the `gleam export package-interface`
//! JSON for that module into this model; the emitters — the codecs module, the
//! activity wrappers, the remote worker stubs, the wire-compat golden, and the
//! emitted `schemas/*.json` artifacts — are all pure functions of it. The model
//! deliberately mirrors the shapes the generated wire supports: records with
//! labelled fields, string-wire enums, scalars, lists, and optional fields.

use std::path::PathBuf;

/// A Gleam type reference inside the generated modules.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GleamType {
    /// `String`.
    String,
    /// `Int`.
    Int,
    /// `Float`.
    Float,
    /// `Bool`.
    Bool,
    /// `List(inner)`.
    List(Box<GleamType>),
    /// A reference to another boundary type in the same types module.
    Named {
        /// The Gleam type name, e.g. `OrderInput`.
        type_name: String,
        /// The derived codec function prefix, e.g. `order_input`.
        fn_prefix: String,
    },
}

/// One record field.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Field {
    /// The constructor label; also the JSON wire property name.
    pub(crate) wire: String,
    /// Field type (wrapped in `option.Option` in the authored type when not
    /// required).
    pub(crate) ty: GleamType,
    /// Whether the field is required on the wire (`false` for
    /// `option.Option(t)` fields, which are omitted when `None`).
    pub(crate) required: bool,
}

/// A record boundary type: a single-constructor custom type whose parameters
/// are all labelled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordDef {
    /// The authored type (and constructor) name.
    pub(crate) type_name: String,
    /// The derived codec function prefix.
    pub(crate) fn_prefix: String,
    /// Fields in declared constructor-parameter order.
    pub(crate) fields: Vec<Field>,
}

/// One enum constructor with its derived wire string.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EnumVariant {
    /// The authored constructor name, e.g. `InputPlacementLocal`.
    pub(crate) constructor: String,
    /// The canonical wire string: `snake_case` of the constructor with the
    /// enum type-name prefix stripped (`InputPlacementLocal` → `local`).
    pub(crate) wire: String,
}

/// An enum boundary type: a multi-constructor custom type whose constructors
/// all carry zero parameters, encoded as its wire string.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EnumDef {
    /// The authored type name.
    pub(crate) type_name: String,
    /// The derived codec function prefix.
    pub(crate) fn_prefix: String,
    /// Variants in declared constructor order.
    pub(crate) variants: Vec<EnumVariant>,
}

/// A boundary type definition.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TypeDef {
    /// A record (single labelled constructor).
    Record(RecordDef),
    /// An enum (multiple zero-arity constructors).
    Enum(EnumDef),
}

impl TypeDef {
    /// The type name of this definition.
    pub(crate) fn type_name(&self) -> &str {
        match self {
            TypeDef::Record(record) => &record.type_name,
            TypeDef::Enum(definition) => &definition.type_name,
        }
    }
}

/// One public type of the authored types module, with everything the emitters
/// need: its emitted schema path, its own definition, and the definitions of
/// every sibling type it references transitively.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundaryType {
    /// The emitted schema artifact path, relative to the project root
    /// (`schemas/<stem>.json`).
    pub(crate) file: PathBuf,
    /// The `snake_case` stem derived from the type name (`order_input`).
    pub(crate) stem: String,
    /// The type itself (always [`GleamType::Named`]).
    pub(crate) root: GleamType,
    /// This type's definition first, then every sibling definition it
    /// references transitively, in depth-first field order.
    pub(crate) defs: Vec<TypeDef>,
}
