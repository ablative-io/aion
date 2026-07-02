//! Resolved activity declarations.
//!
//! A [`ActivityDeclaration`](super::declaration::ActivityDeclaration) names its
//! input and output value types as strings (for example `OrderInput`). Before
//! the emitters can generate wrappers, worker stubs, and goldens, each type
//! name is resolved to the [`BoundaryType`] the types module declares under
//! that name. The resolved view borrows both the declaration and the model,
//! so resolution allocates only the short type/prefix strings.

use super::declaration::ActivityDeclaration;
use super::model::BoundaryType;

/// A declared value type resolved to the boundary type that generates its
/// codec.
pub(crate) struct ResolvedType<'a> {
    /// The Gleam type name, e.g. `OrderInput`.
    pub(crate) gleam_type: String,
    /// The generated codec/function prefix, e.g. `order_input`.
    pub(crate) fn_prefix: String,
    /// The boundary type whose record or enum structure backs the value; the
    /// worker and golden emitters read its definitions.
    pub(crate) boundary: &'a BoundaryType,
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
