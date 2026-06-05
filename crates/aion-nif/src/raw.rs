//! Safe wrappers for process-heap term construction.
//!
//! This module is the crate's FFI seam for heap-backed beamr terms. It exposes
//! safe Rust functions and currently relies only on beamr's public safe APIs.
//! aion-nif intentionally avoids permanent per-call heap leaks here: that is the
//! beamr-meridian limitation this crate supersedes, and any future raw allocation
//! path must remain isolated behind this module's documented wrappers.

use beamr::term::{Term, binary, boxed};
use std::sync::{Mutex, OnceLock};

use crate::TermError;

static TERM_STORAGE: OnceLock<Mutex<Vec<Box<[u64]>>>> = OnceLock::new();

fn keep_owned_term<F>(word_len: usize, shape: &'static str, write: F) -> Result<Term, TermError>
where
    F: FnOnce(&mut [u64]) -> Result<Term, TermError>,
{
    let mut heap = vec![0_u64; word_len].into_boxed_slice();
    let term = write(&mut heap)?;
    let storage = TERM_STORAGE.get_or_init(|| Mutex::new(Vec::new()));
    storage
        .lock()
        .map_err(|_| TermError::HeapAllocation { shape })?
        .push(heap);
    Ok(term)
}

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

/// Builds a binary term backed by storage retained by the raw allocation seam.
///
/// This is the allocation path used by [`crate::IntoTerm`], whose signature does
/// not expose caller-owned heap storage. The storage is retained so returned
/// heap-backed terms remain readable after conversion returns.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the binary layout cannot be
/// written or retained storage cannot be locked.
pub fn owned_binary_term(bytes: &[u8]) -> Result<Term, TermError> {
    keep_owned_term(binary_word_len(bytes.len()), "binary", |heap| {
        binary_term(heap, bytes)
    })
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

/// Builds a tuple term backed by storage retained by the raw allocation seam.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the tuple layout cannot be
/// written or retained storage cannot be locked.
pub fn owned_tuple_term(elements: &[Term]) -> Result<Term, TermError> {
    keep_owned_term(tuple_word_len(elements.len()), "tuple", |heap| {
        tuple_term(heap, elements)
    })
}

/// Returns the number of heap words required for [`float_term`].
#[must_use]
pub const fn float_word_len() -> usize {
    2
}

/// Builds a float term in caller-provided process-heap words.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the supplied heap slice is too
/// small for the float layout.
pub fn float_term(heap: &mut [u64], value: f64) -> Result<Term, TermError> {
    boxed::write_float(heap, value).ok_or(TermError::HeapAllocation { shape: "float" })
}

/// Builds a float term backed by storage retained by the raw allocation seam.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the float layout cannot be
/// written or retained storage cannot be locked.
pub fn owned_float_term(value: f64) -> Result<Term, TermError> {
    keep_owned_term(float_word_len(), "float", |heap| float_term(heap, value))
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

/// Builds a cons cell backed by storage retained by the raw allocation seam.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the cons layout cannot be written
/// or retained storage cannot be locked.
pub fn owned_cons_term(head: Term, tail: Term) -> Result<Term, TermError> {
    keep_owned_term(cons_word_len(), "cons", |heap| cons_term(heap, head, tail))
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

/// Builds a proper list backed by storage retained by the raw allocation seam.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the list layout cannot be written
/// or retained storage cannot be locked.
pub fn owned_list_term(elements: &[Term]) -> Result<Term, TermError> {
    keep_owned_term(list_word_len(elements.len()), "list", |heap| {
        list_term(heap, elements)
    })
}

/// Returns the number of heap words required for [`map_term`].
#[must_use]
pub const fn map_word_len(len: usize) -> usize {
    2 + (len * 2)
}

/// Builds a map term in caller-provided process-heap words.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the supplied heap slice is too
/// small for the map layout or key/value counts differ.
pub fn map_term(heap: &mut [u64], keys: &[Term], values: &[Term]) -> Result<Term, TermError> {
    boxed::write_map(heap, keys, values).ok_or(TermError::HeapAllocation { shape: "map" })
}

/// Builds a map term backed by storage retained by the raw allocation seam.
///
/// # Errors
///
/// Returns [`TermError::HeapAllocation`] when the map layout cannot be written
/// or retained storage cannot be locked.
pub fn owned_map_term(keys: &[Term], values: &[Term]) -> Result<Term, TermError> {
    keep_owned_term(map_word_len(keys.len()), "map", |heap| {
        map_term(heap, keys, values)
    })
}

#[cfg(test)]
mod tests {
    use beamr::term::{Term, binary::Binary, boxed::Cons, boxed::Float, boxed::Map, boxed::Tuple};

    use super::{
        binary_term, binary_word_len, float_term, float_word_len, list_term, list_word_len,
        map_term, map_word_len, tuple_term, tuple_word_len,
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

    #[test]
    fn float_wrapper_round_trips_value() -> Result<(), Box<dyn std::error::Error>> {
        let mut heap = vec![0_u64; float_word_len()];
        let term = float_term(&mut heap, 3.5)?;
        let float = Float::new(term).ok_or("float accessor should accept float term")?;

        assert_eq!(float.value().to_bits(), 3.5_f64.to_bits());

        Ok(())
    }

    #[test]
    fn map_wrapper_round_trips_key_values() -> Result<(), Box<dyn std::error::Error>> {
        let keys = [Term::small_int(1), Term::small_int(2)];
        let values = [Term::small_int(10), Term::small_int(20)];
        let mut heap = vec![0_u64; map_word_len(keys.len())];
        let term = map_term(&mut heap, &keys, &values)?;
        let map = Map::new(term).ok_or("map accessor should accept map term")?;

        assert_eq!(map.len(), 2);
        assert_eq!(map.get(Term::small_int(1)), Some(Term::small_int(10)));
        assert_eq!(map.get(Term::small_int(2)), Some(Term::small_int(20)));

        Ok(())
    }
}
