//! `FromTerm`/`IntoTerm` traits and primitive implementations.
//!
//! [`FromTerm`] decodes typed NIF arguments from beamr [`Term`] values and
//! [`IntoTerm`] encodes typed NIF return values back into beamr terms. These
//! traits keep raw term internals out of author code; the generated declaration
//! shims use them with static dispatch instead of passing raw term slices to NIF
//! bodies.

use beamr::{
    atom::Atom,
    native::ProcessContext,
    term::{Term, binary::Binary, boxed::Float},
};

use crate::{NifContext, TermError, raw};

/// Decodes a beamr term into a Rust value for a typed NIF argument.
///
/// This trait is intended for static dispatch by declaration shims. It returns
/// `Self`, so it is not a trait-object API.
pub trait FromTerm: Sized {
    /// Decode `term` using process context services such as atom resolution.
    ///
    /// # Errors
    ///
    /// Returns [`TermError`] when the runtime term kind or contents do not match
    /// the requested Rust type.
    fn from_term(term: Term, ctx: &ProcessContext) -> Result<Self, TermError>;
}

/// Encodes a Rust value as a beamr term for a typed NIF return value.
///
/// This trait is intended for static dispatch by declaration shims. Heap-backed
/// term allocation is routed through the raw allocation seam instead of exposing
/// raw term internals to callers.
pub trait IntoTerm {
    /// Encode `self` as a term associated with `ctx`.
    ///
    /// # Errors
    ///
    /// Returns [`TermError`] when heap allocation or atom interning support is
    /// unavailable.
    fn into_term(self, ctx: &mut NifContext<'_>) -> Result<Term, TermError>;
}

/// Owned atom name used for typed atom conversion.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AtomName(String);

impl AtomName {
    /// Creates an atom-name wrapper.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Borrows the wrapped atom name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the atom name.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<String> for AtomName {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for AtomName {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

fn table_missing(atom: impl Into<String>) -> TermError {
    TermError::AtomResolution {
        atom: atom.into(),
        reason: "atom table is unavailable".to_owned(),
    }
}

fn term_kind(term: Term) -> String {
    if term.is_nil() {
        "nil".to_owned()
    } else if term.is_small_int() {
        "integer".to_owned()
    } else if term.is_atom() {
        "atom".to_owned()
    } else if Binary::new(term).is_some() {
        "binary".to_owned()
    } else if Float::new(term).is_some() {
        "float".to_owned()
    } else if term.is_list() {
        "list".to_owned()
    } else if term.is_boxed() {
        "boxed".to_owned()
    } else {
        format!("{:?}", term.tag())
    }
}

pub(crate) fn mismatch(expected: &'static str, term: Term) -> TermError {
    TermError::TypeMismatch {
        expected,
        found: term_kind(term),
    }
}

fn small_int_term(value: i64, ctx: &mut NifContext<'_>) -> Result<Term, TermError> {
    Term::try_small_int(value)
        .map(|term| ctx.process_mut().allocate_term(term))
        .ok_or(TermError::HeapAllocation { shape: "integer" })
}

impl FromTerm for i64 {
    fn from_term(term: Term, _ctx: &ProcessContext) -> Result<Self, TermError> {
        term.as_small_int().ok_or_else(|| mismatch("integer", term))
    }
}

impl IntoTerm for i64 {
    fn into_term(self, ctx: &mut NifContext<'_>) -> Result<Term, TermError> {
        small_int_term(self, ctx)
    }
}

impl FromTerm for u64 {
    fn from_term(term: Term, _ctx: &ProcessContext) -> Result<Self, TermError> {
        let value = term
            .as_small_int()
            .ok_or_else(|| mismatch("integer", term))?;
        u64::try_from(value).map_err(|_| mismatch("unsigned integer", term))
    }
}

impl IntoTerm for u64 {
    fn into_term(self, ctx: &mut NifContext<'_>) -> Result<Term, TermError> {
        let value =
            i64::try_from(self).map_err(|_| TermError::HeapAllocation { shape: "integer" })?;
        small_int_term(value, ctx)
    }
}

impl FromTerm for f64 {
    fn from_term(term: Term, _ctx: &ProcessContext) -> Result<Self, TermError> {
        Float::new(term)
            .map(beamr::term::boxed::Float::value)
            .ok_or_else(|| mismatch("float", term))
    }
}

impl IntoTerm for f64 {
    fn into_term(self, ctx: &mut NifContext<'_>) -> Result<Term, TermError> {
        raw::owned_float_term(ctx, self)
    }
}

impl FromTerm for bool {
    fn from_term(term: Term, _ctx: &ProcessContext) -> Result<Self, TermError> {
        match term.as_atom() {
            Some(Atom::TRUE) => Ok(true),
            Some(Atom::FALSE) => Ok(false),
            _ => Err(mismatch("boolean atom", term)),
        }
    }
}

impl IntoTerm for bool {
    fn into_term(self, ctx: &mut NifContext<'_>) -> Result<Term, TermError> {
        let atom = if self { Atom::TRUE } else { Atom::FALSE };
        Ok(ctx.process_mut().allocate_term(Term::atom(atom)))
    }
}

impl FromTerm for Vec<u8> {
    fn from_term(term: Term, _ctx: &ProcessContext) -> Result<Self, TermError> {
        Binary::new(term)
            .map(|binary| binary.as_bytes().to_vec())
            .ok_or_else(|| mismatch("binary", term))
    }
}

impl IntoTerm for Vec<u8> {
    fn into_term(self, ctx: &mut NifContext<'_>) -> Result<Term, TermError> {
        raw::owned_binary_term(ctx, &self)
    }
}

impl FromTerm for String {
    fn from_term(term: Term, ctx: &ProcessContext) -> Result<Self, TermError> {
        let bytes = Vec::<u8>::from_term(term, ctx)?;
        String::from_utf8(bytes).map_err(|_| mismatch("utf8 binary", term))
    }
}

impl IntoTerm for String {
    fn into_term(self, ctx: &mut NifContext<'_>) -> Result<Term, TermError> {
        raw::owned_binary_term(ctx, self.as_bytes())
    }
}

impl FromTerm for AtomName {
    fn from_term(term: Term, ctx: &ProcessContext) -> Result<Self, TermError> {
        let atom = term.as_atom().ok_or_else(|| mismatch("atom", term))?;
        let table = ctx
            .atom_table()
            .ok_or_else(|| table_missing(format!("{atom:?}")))?;
        table
            .resolve(atom)
            .map(|name| Self(name.to_owned()))
            .ok_or_else(|| TermError::AtomResolution {
                atom: format!("{atom:?}"),
                reason: "atom is not present in the process atom table".to_owned(),
            })
    }
}

impl IntoTerm for AtomName {
    fn into_term(self, ctx: &mut NifContext<'_>) -> Result<Term, TermError> {
        let atom = {
            let table = ctx
                .process()
                .atom_table()
                .ok_or_else(|| table_missing(self.0.clone()))?;
            table.intern(&self.0)
        };
        Ok(ctx.process_mut().allocate_term(Term::atom(atom)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use beamr::{atom::AtomTable, native::ProcessContext, term::Term};

    use super::{AtomName, FromTerm, IntoTerm};
    use crate::TermError;

    fn context() -> ProcessContext {
        let mut ctx = ProcessContext::new();
        ctx.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
        ctx
    }

    #[test]
    fn primitive_values_round_trip() -> Result<(), TermError> {
        let mut ctx = context();
        let mut nif_ctx = crate::NifContext::new(&mut ctx);

        let signed = i64::from_term((-42_i64).into_term(&mut nif_ctx)?, nif_ctx.process())?;
        assert_eq!(signed, -42);

        let unsigned = u64::from_term(42_u64.into_term(&mut nif_ctx)?, nif_ctx.process())?;
        assert_eq!(unsigned, 42);

        let float = f64::from_term(3.5_f64.into_term(&mut nif_ctx)?, nif_ctx.process())?;
        assert_eq!(float.to_bits(), 3.5_f64.to_bits());

        let flag = bool::from_term(true.into_term(&mut nif_ctx)?, nif_ctx.process())?;
        assert!(flag);

        let text = String::from_term(
            "hello".to_owned().into_term(&mut nif_ctx)?,
            nif_ctx.process(),
        )?;
        assert_eq!(text, "hello");

        let bytes =
            Vec::<u8>::from_term(vec![1_u8, 2, 3].into_term(&mut nif_ctx)?, nif_ctx.process())?;
        assert_eq!(bytes, vec![1, 2, 3]);

        let atom = AtomName::from_term(
            AtomName::from("ready").into_term(&mut nif_ctx)?,
            nif_ctx.process(),
        )?;
        assert_eq!(atom, AtomName::from("ready"));

        Ok(())
    }

    #[test]
    fn wrong_kind_decode_returns_typed_error() {
        let ctx = context();
        let error = i64::from_term(Term::NIL, &ctx);
        assert!(matches!(
            error,
            Err(TermError::TypeMismatch {
                expected: "integer",
                ..
            })
        ));
    }
}
