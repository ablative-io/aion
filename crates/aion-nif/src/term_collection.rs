//! Structural term conversions for options, results, lists, and maps.

use std::collections::BTreeMap;

use beamr::{
    atom::Atom,
    native::ProcessContext,
    term::{Term, boxed::Cons, boxed::Map, boxed::Tuple},
};

use crate::{FromTerm, IntoTerm, NifContext, TermError, raw, term::mismatch};

/// Marker for values that may use the homogeneous-list `Vec<T>` term shape.
///
/// `Vec<u8>` is reserved by AN-002 for binary terms, so the blanket list
/// conversion is intentionally limited to supported element types other than
/// `u8` on stable Rust.
pub trait ListElement: FromTerm + IntoTerm {}

impl ListElement for i64 {}
impl ListElement for u64 {}
impl ListElement for f64 {}
impl ListElement for bool {}
impl ListElement for String {}
impl ListElement for crate::AtomName {}

impl<T> FromTerm for Option<T>
where
    T: FromTerm,
{
    fn from_term(term: Term, ctx: &ProcessContext) -> Result<Self, TermError> {
        if term.is_nil() {
            Ok(None)
        } else {
            T::from_term(term, ctx).map(Some)
        }
    }
}

impl<T> IntoTerm for Option<T>
where
    T: IntoTerm,
{
    fn into_term(self, ctx: &mut NifContext<'_, '_>) -> Result<Term, TermError> {
        match self {
            Some(value) => value.into_term(ctx),
            None => Ok(ctx.process_mut().allocate_term(Term::NIL)),
        }
    }
}

impl<T, E> FromTerm for Result<T, E>
where
    T: FromTerm,
    E: FromTerm,
{
    fn from_term(term: Term, ctx: &ProcessContext) -> Result<Self, TermError> {
        let tuple = Tuple::new(term).ok_or_else(|| mismatch("result tuple", term))?;
        if tuple.arity() != 2 {
            return Err(mismatch("result tuple", term));
        }

        let tag = tuple.get(0).ok_or_else(|| mismatch("result tuple", term))?;
        let value = tuple.get(1).ok_or_else(|| mismatch("result tuple", term))?;
        match tag.as_atom() {
            Some(Atom::OK) => T::from_term(value, ctx).map(Ok),
            Some(Atom::ERROR) => E::from_term(value, ctx).map(Err),
            _ => Err(mismatch("ok/error result tag", tag)),
        }
    }
}

impl<T, E> IntoTerm for Result<T, E>
where
    T: IntoTerm,
    E: IntoTerm,
{
    fn into_term(self, ctx: &mut NifContext<'_, '_>) -> Result<Term, TermError> {
        let (tag, value) = match self {
            Ok(value) => (Term::atom(Atom::OK), value.into_term(ctx)?),
            Err(error) => (Term::atom(Atom::ERROR), error.into_term(ctx)?),
        };
        let tag = ctx.process_mut().allocate_term(tag);
        raw::owned_tuple_term(ctx, &[tag, value])
    }
}

impl<T> FromTerm for Vec<T>
where
    T: ListElement,
{
    fn from_term(term: Term, ctx: &ProcessContext) -> Result<Self, TermError> {
        let mut values = Vec::new();
        let mut tail = term;

        while !tail.is_nil() {
            let cons = Cons::new(tail).ok_or_else(|| mismatch("proper list", tail))?;
            values.push(T::from_term(cons.head(), ctx)?);
            tail = cons.tail();
        }

        Ok(values)
    }
}

impl<T> IntoTerm for Vec<T>
where
    T: ListElement,
{
    fn into_term(self, ctx: &mut NifContext<'_, '_>) -> Result<Term, TermError> {
        let terms = self
            .into_iter()
            .map(|value| value.into_term(ctx))
            .collect::<Result<Vec<_>, _>>()?;
        raw::owned_list_term(ctx, &terms)
    }
}

impl<T> FromTerm for BTreeMap<String, T>
where
    T: FromTerm,
{
    fn from_term(term: Term, ctx: &ProcessContext) -> Result<Self, TermError> {
        let map = Map::new(term).ok_or_else(|| mismatch("map", term))?;
        let mut values = BTreeMap::new();

        for index in 0..map.len() {
            let key_term = map.key(index).ok_or_else(|| mismatch("map", term))?;
            let value_term = map.value(index).ok_or_else(|| mismatch("map", term))?;
            let key = String::from_term(key_term, ctx)?;
            let value = T::from_term(value_term, ctx)?;
            values.insert(key, value);
        }

        Ok(values)
    }
}

impl<T> IntoTerm for BTreeMap<String, T>
where
    T: IntoTerm,
{
    fn into_term(self, ctx: &mut NifContext<'_, '_>) -> Result<Term, TermError> {
        let mut keys = Vec::with_capacity(self.len());
        let mut values = Vec::with_capacity(self.len());

        for (key, value) in self {
            keys.push(key.into_term(ctx)?);
            values.push(value.into_term(ctx)?);
        }

        raw::owned_map_term(ctx, &keys, &values)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use beamr::{atom::AtomTable, native::ProcessContext};

    use super::*;

    fn context() -> ProcessContext<'static> {
        let mut ctx = ProcessContext::new();
        ctx.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
        ctx
    }

    #[test]
    fn option_values_round_trip() -> Result<(), TermError> {
        let mut ctx = context();
        let mut nif_ctx = crate::NifContext::new(&mut ctx);

        let none =
            Option::<i64>::from_term(None::<i64>.into_term(&mut nif_ctx)?, nif_ctx.process())?;
        assert_eq!(none, None);

        let some =
            Option::<i64>::from_term(Some(7_i64).into_term(&mut nif_ctx)?, nif_ctx.process())?;
        assert_eq!(some, Some(7));

        Ok(())
    }

    #[test]
    fn result_values_round_trip() -> Result<(), TermError> {
        let mut ctx = context();
        let mut nif_ctx = crate::NifContext::new(&mut ctx);

        let ok = Result::<i64, String>::from_term(
            Result::<i64, String>::Ok(7_i64).into_term(&mut nif_ctx)?,
            nif_ctx.process(),
        )?;
        assert_eq!(ok, Ok(7));

        let error = Result::<i64, String>::from_term(
            Result::<i64, String>::Err("failed".to_owned()).into_term(&mut nif_ctx)?,
            nif_ctx.process(),
        )?;
        assert_eq!(error, Err("failed".to_owned()));

        Ok(())
    }

    #[test]
    fn list_values_round_trip() -> Result<(), TermError> {
        let mut ctx = context();
        let mut nif_ctx = crate::NifContext::new(&mut ctx);

        let list = Vec::<i64>::from_term(
            vec![1_i64, 2, 3].into_term(&mut nif_ctx)?,
            nif_ctx.process(),
        )?;
        assert_eq!(list, vec![1, 2, 3]);

        Ok(())
    }

    #[test]
    fn map_values_round_trip() -> Result<(), TermError> {
        let mut ctx = context();
        let mut nif_ctx = crate::NifContext::new(&mut ctx);
        let mut original = BTreeMap::new();
        original.insert("one".to_owned(), 1_i64);
        original.insert("two".to_owned(), 2_i64);

        let decoded = BTreeMap::<String, i64>::from_term(
            original.clone().into_term(&mut nif_ctx)?,
            nif_ctx.process(),
        )?;
        assert_eq!(decoded, original);

        Ok(())
    }
}
