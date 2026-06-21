//! Resolved activity declarations.
//!
//! A [`ActivityDeclaration`](super::declaration::ActivityDeclaration) names its
//! input and output value types as strings (for example `OrderInput`). Before
//! the emitters can generate codecs, wrappers, worker stubs, and goldens, each
//! type name is resolved to the parsed [`SchemaArtifact`] whose generated Gleam
//! type carries that name. The resolved view borrows both the declaration and
//! the artifacts, so resolution allocates only the short type/prefix strings.

use super::declaration::ActivityDeclaration;
use super::schema::SchemaArtifact;

/// A declared value type resolved to the schema artifact that generates it.
pub(crate) struct ResolvedType<'a> {
    /// The generated Gleam type name, e.g. `OrderInput`.
    pub(crate) gleam_type: String,
    /// The generated codec/function prefix, e.g. `order_input`.
    pub(crate) fn_prefix: String,
    /// The schema artifact whose record or enum structure backs the type; the
    /// worker and golden emitters read its fields.
    pub(crate) artifact: &'a SchemaArtifact,
}

/// An activity declaration with its input and output value types resolved.
pub(crate) struct ResolvedActivity<'a> {
    /// The validated declaration (name, tier, type names).
    pub(crate) declaration: &'a ActivityDeclaration,
    /// The resolved input value type.
    pub(crate) input: ResolvedType<'a>,
    /// The resolved output value type.
    pub(crate) output: ResolvedType<'a>,
}
