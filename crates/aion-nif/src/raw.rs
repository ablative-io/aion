//! Safe wrappers for process-heap term construction.
//!
//! This module is the crate's FFI seam for heap-backed beamr terms. It exposes
//! safe Rust functions and currently relies only on beamr's public safe APIs.
//! aion-nif intentionally avoids permanent per-call heap leaks here: that is the
//! beamr-meridian limitation this crate supersedes, and any future raw allocation
//! path must remain isolated behind this module's documented wrappers.

use beamr::{
    native::ProcessContext,
    term::{Term, binary},
};

use crate::TermError;

/// Builds a binary term in caller-provided process-heap words.
///
/// beamr exposes binary construction as a safe writer over a heap word slice,
/// rather than as a `ProcessContext` allocator. The caller owns the heap storage
/// and must keep it alive while the returned term is read.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the supplied heap slice is too
/// small for the binary layout.
pub fn binary_term(heap: &mut [u64], bytes: &[u8]) -> Result<Term, TermError> {
    binary::write_binary(heap, bytes).ok_or(TermError::HeapAllocation { shape: "binary" })
}

/// Returns the number of heap words required for [`binary_term`].
#[must_use]
pub const fn binary_word_len(byte_len: usize) -> usize {
    2 + binary::packed_word_count(byte_len)
}

/// Builds a tuple term through beamr's safe `ProcessContext` allocator.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when beamr cannot allocate the tuple
/// layout.
pub fn tuple_term(context: &mut ProcessContext, elements: &[Term]) -> Result<Term, TermError> {
    context
        .alloc_tuple(elements)
        .map_err(|_error| TermError::HeapAllocation { shape: "tuple" })
}

/// Builds a cons cell term through beamr's safe `ProcessContext` allocator.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when beamr cannot allocate the cons
/// cell layout.
pub fn cons_term(context: &mut ProcessContext, head: Term, tail: Term) -> Result<Term, TermError> {
    context
        .alloc_cons(head, tail)
        .map_err(|_error| TermError::HeapAllocation { shape: "cons" })
}

/// Builds a proper list by consing terms right-to-left.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when beamr cannot allocate any cons
/// cell in the list.
pub fn list_term(context: &mut ProcessContext, elements: &[Term]) -> Result<Term, TermError> {
    elements
        .iter()
        .rev()
        .try_fold(Term::NIL, |tail, head| cons_term(context, *head, tail))
}

#[cfg(test)]
mod tests {
    use beamr::{
        native::ProcessContext,
        term::{Term, binary::Binary, boxed::Cons, boxed::Tuple},
    };

    use super::{binary_term, binary_word_len, list_term, tuple_term};

    #[test]
    fn tuple_wrapper_round_trips_elements() -> Result<(), Box<dyn std::error::Error>> {
        let mut context = ProcessContext::new();
        let elements = [Term::small_int(1), Term::small_int(2)];
        let term = tuple_term(&mut context, &elements)?;
        let tuple = Tuple::new(term).ok_or("tuple accessor should accept tuple term")?;

        assert_eq!(tuple.arity(), 2);
        assert_eq!(tuple.get(0), Some(Term::small_int(1)));
        assert_eq!(tuple.get(1), Some(Term::small_int(2)));

        Ok(())
    }

    #[test]
    fn list_wrapper_round_trips_cons_cells() -> Result<(), Box<dyn std::error::Error>> {
        let mut context = ProcessContext::new();
        let term = list_term(&mut context, &[Term::small_int(10), Term::small_int(20)])?;
        let first = Cons::new(term).ok_or("first cons cell should exist")?;
        let second = Cons::new(first.tail()).ok_or("second cons cell should exist")?;

        assert_eq!(first.head(), Term::small_int(10));
        assert_eq!(second.head(), Term::small_int(20));
        assert_eq!(second.tail(), Term::NIL);

        Ok(())
    }

    #[test]
    fn binary_wrapper_round_trips_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let bytes = b"ok";
        let mut heap = vec![0_u64; binary_word_len(bytes.len())];
        let term = binary_term(&mut heap, bytes)?;
        let binary = Binary::new(term).ok_or("binary accessor should accept binary term")?;

        assert_eq!(binary.as_bytes(), bytes);

        Ok(())
    }
}
