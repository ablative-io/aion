//! Safe wrappers for process-heap term construction.
//!
//! This module is the crate's FFI seam for heap-backed beamr terms. It exposes
//! safe Rust functions and currently relies only on beamr's public safe APIs.
//! aion-nif intentionally avoids permanent per-call heap leaks here: that is the
//! beamr-meridian limitation this crate supersedes, and any future raw allocation
//! path must remain isolated behind this module's documented wrappers.

use beamr::term::{Term, binary, boxed};

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

/// Returns the number of heap words required for [`tuple_term`].
#[must_use]
pub const fn tuple_word_len(arity: usize) -> usize {
    1 + arity
}

/// Builds a tuple term in caller-provided process-heap words.
///
/// The caller owns the heap storage and must keep it alive while the returned
/// term is read. This wrapper intentionally uses beamr's heap-slice writer
/// rather than `ProcessContext::alloc_tuple`, because beamr 0.3.1 implements
/// that convenience allocator with permanent leaked heap storage.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the supplied heap slice is too
/// small for the tuple layout.
pub fn tuple_term(heap: &mut [u64], elements: &[Term]) -> Result<Term, TermError> {
    boxed::write_tuple(heap, elements).ok_or(TermError::HeapAllocation { shape: "tuple" })
}

/// Returns the number of heap words required for one [`cons_term`].
#[must_use]
pub const fn cons_word_len() -> usize {
    2
}

/// Builds a cons cell term in caller-provided process-heap words.
///
/// The caller owns the heap storage and must keep it alive while the returned
/// term is read. This wrapper intentionally uses beamr's heap-slice writer
/// rather than `ProcessContext::alloc_cons`, because beamr 0.3.1 implements
/// that convenience allocator with permanent leaked heap storage.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the supplied heap slice is too
/// small for the cons-cell layout.
pub fn cons_term(words: &mut [u64], head: Term, tail: Term) -> Result<Term, TermError> {
    boxed::write_cons(words, head, tail).ok_or(TermError::HeapAllocation { shape: "cons" })
}

/// Returns the number of heap words required for [`list_term`].
#[must_use]
pub const fn list_word_len(len: usize) -> usize {
    cons_word_len() * len
}

/// Builds a proper list by consing terms right-to-left.
///
/// The caller owns the heap storage for all cons cells and must keep it alive
/// while the returned list is read.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the supplied heap slice is too
/// small for every cons cell in the list.
pub fn list_term(heap: &mut [u64], elements: &[Term]) -> Result<Term, TermError> {
    if heap.len() < list_word_len(elements.len()) {
        return Err(TermError::HeapAllocation { shape: "list" });
    }

    elements
        .iter()
        .rev()
        .zip(heap.chunks_exact_mut(cons_word_len()).rev())
        .try_fold(Term::NIL, |tail, (head, cell_heap)| {
            cons_term(cell_heap, *head, tail)
        })
}

#[cfg(test)]
mod tests {
    use beamr::term::{Term, binary::Binary, boxed::Cons, boxed::Tuple};

    use super::{
        binary_term, binary_word_len, list_term, list_word_len, tuple_term, tuple_word_len,
    };

    #[test]
    fn tuple_wrapper_round_trips_elements() -> Result<(), Box<dyn std::error::Error>> {
        let elements = [Term::small_int(1), Term::small_int(2)];
        let mut heap = vec![0_u64; tuple_word_len(elements.len())];
        let term = tuple_term(&mut heap, &elements)?;
        let tuple = Tuple::new(term).ok_or("tuple accessor should accept tuple term")?;

        assert_eq!(tuple.arity(), 2);
        assert_eq!(tuple.get(0), Some(Term::small_int(1)));
        assert_eq!(tuple.get(1), Some(Term::small_int(2)));

        Ok(())
    }

    #[test]
    fn list_wrapper_round_trips_cons_cells() -> Result<(), Box<dyn std::error::Error>> {
        let mut heap = vec![0_u64; list_word_len(2)];
        let term = list_term(&mut heap, &[Term::small_int(10), Term::small_int(20)])?;
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
