//! Per-invocation context for typed NIF term conversion.
//!
//! `NifContext` wraps beamr's [`ProcessContext`] and owns heap slices retained
//! while a generated NIF shim is running. Dropping the context drops every
//! retained slice, so heap-backed terms produced by `IntoTerm` are scoped to one
//! native invocation instead of global process state.

use beamr::{native::ProcessContext, term::Term};

use crate::TermError;

/// Scoped context used by generated NIF shims during one native invocation.
pub struct NifContext<'ctx> {
    process: &'ctx mut ProcessContext,
    retained_heap: Vec<Box<[u64]>>,
}

impl<'ctx> NifContext<'ctx> {
    /// Creates a context whose retained heap storage is dropped with `self`.
    #[must_use]
    pub const fn new(process: &'ctx mut ProcessContext) -> Self {
        Self {
            process,
            retained_heap: Vec::new(),
        }
    }

    /// Borrows the wrapped process context for atom resolution and term reads.
    #[must_use]
    pub fn process(&self) -> &ProcessContext {
        self.process
    }

    /// Mutably borrows the wrapped process context for immediate term allocation.
    pub fn process_mut(&mut self) -> &mut ProcessContext {
        self.process
    }

    /// Allocates and retains a heap slice for a heap-backed term shape.
    ///
    /// # Errors
    ///
    /// Returns the conversion error reported by `write` when the requested term
    /// shape cannot be written into the retained heap slice.
    pub fn retain_heap<F>(&mut self, word_len: usize, write: F) -> Result<Term, TermError>
    where
        F: FnOnce(&mut [u64]) -> Result<Term, TermError>,
    {
        let mut heap = vec![0_u64; word_len].into_boxed_slice();
        let term = write(&mut heap)?;
        self.retained_heap.push(heap);
        Ok(term)
    }

    #[cfg(test)]
    pub(crate) fn retained_heap_count(&self) -> usize {
        self.retained_heap.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use beamr::{atom::AtomTable, native::ProcessContext, term::binary::Binary};

    use super::NifContext;
    use crate::{IntoTerm, TermError};

    fn context() -> ProcessContext {
        let mut ctx = ProcessContext::new();
        ctx.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
        ctx
    }

    #[test]
    fn retained_heap_storage_is_scoped_to_context_drop() -> Result<(), TermError> {
        let mut process = context();

        {
            let mut ctx = NifContext::new(&mut process);
            let term = "retained".to_owned().into_term(&mut ctx)?;
            let binary = Binary::new(term).ok_or(TermError::HeapAllocation { shape: "binary" })?;

            assert_eq!(binary.as_bytes(), b"retained");
            assert_eq!(ctx.retained_heap_count(), 1);
        }

        let next_count = {
            let ctx = NifContext::new(&mut process);
            ctx.retained_heap_count()
        };
        assert_eq!(next_count, 0);

        Ok(())
    }
}
